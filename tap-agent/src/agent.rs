// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use indexer_common::prelude::{
    escrow_accounts, indexer_allocations, DeploymentDetails, SubgraphClient,
};
use ractor::{Actor, ActorRef};

use crate::agent::sender_accounts_manager::{
    SenderAccountsManagerArgs, SenderAccountsManagerMessage,
};
use crate::config::{Cli, EscrowSubgraph, Ethereum, IndexerInfrastructure, NetworkSubgraph, Tap};
use crate::{aggregator_endpoints, database, CONFIG, EIP_712_DOMAIN};
use sender_accounts_manager::SenderAccountsManager;

mod allocation_id_tracker;
mod sender_account;
pub mod sender_accounts_manager;
mod sender_allocation;
mod unaggregated_receipts;

pub async fn start_agent() -> ActorRef<SenderAccountsManagerMessage> {
    let Cli {
        ethereum: Ethereum { indexer_address },
        indexer_infrastructure:
            IndexerInfrastructure {
                graph_node_query_endpoint,
                graph_node_status_endpoint,
                ..
            },
        postgres,
        network_subgraph:
            NetworkSubgraph {
                network_subgraph_deployment,
                network_subgraph_endpoint,
                allocation_syncing_interval_ms,
            },
        escrow_subgraph:
            EscrowSubgraph {
                escrow_subgraph_deployment,
                escrow_subgraph_endpoint,
                escrow_syncing_interval_ms,
            },
        tap: Tap {
            sender_aggregator_endpoints_file,
            ..
        },
        ..
    } = &*CONFIG;
    let pgpool = database::connect(postgres).await;

    let http_client = reqwest::Client::new();

    let network_subgraph = Box::leak(Box::new(SubgraphClient::new(
        http_client.clone(),
        network_subgraph_deployment
            .map(|deployment| {
                DeploymentDetails::for_graph_node(
                    graph_node_status_endpoint,
                    graph_node_query_endpoint,
                    deployment,
                )
            })
            .transpose()
            .expect("Failed to parse graph node query endpoint and network subgraph deployment"),
        DeploymentDetails::for_query_url(network_subgraph_endpoint)
            .expect("Failed to parse network subgraph endpoint"),
    )));

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        *indexer_address,
        1,
        Duration::from_millis(*allocation_syncing_interval_ms),
    );

    let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
        http_client.clone(),
        escrow_subgraph_deployment
            .map(|deployment| {
                DeploymentDetails::for_graph_node(
                    graph_node_status_endpoint,
                    graph_node_query_endpoint,
                    deployment,
                )
            })
            .transpose()
            .expect("Failed to parse graph node query endpoint and escrow subgraph deployment"),
        DeploymentDetails::for_query_url(escrow_subgraph_endpoint)
            .expect("Failed to parse escrow subgraph endpoint"),
    )));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        *indexer_address,
        Duration::from_millis(*escrow_syncing_interval_ms),
        false,
    );

    // TODO: replace with a proper implementation once the gateway registry contract is ready
    let sender_aggregator_endpoints =
        aggregator_endpoints::load_aggregator_endpoints(sender_aggregator_endpoints_file.clone());

    let args = SenderAccountsManagerArgs {
        config: &CONFIG,
        domain_separator: EIP_712_DOMAIN.clone(),
        pgpool,
        indexer_allocations,
        escrow_accounts,
        escrow_subgraph,
        sender_aggregator_endpoints,
    };

    let (manager, _) = SenderAccountsManager::spawn(None, SenderAccountsManager, args)
        .await
        .expect("Failed to start sender accounts manager actor.");
    manager
}
