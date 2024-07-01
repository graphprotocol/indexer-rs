// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use indexer_common::prelude::{
    escrow_accounts, indexer_allocations, DeploymentDetails, SubgraphClient,
};
use ractor::concurrency::JoinHandle;
use ractor::{Actor, ActorRef};

use crate::agent::sender_accounts_manager::{
    SenderAccountsManagerArgs, SenderAccountsManagerMessage,
};
use crate::config::{
    Config, EscrowSubgraph, Ethereum, IndexerInfrastructure, NetworkSubgraph, Tap,
};
use crate::{database, CONFIG, EIP_712_DOMAIN};
use sender_accounts_manager::SenderAccountsManager;

pub mod sender_account;
pub mod sender_accounts_manager;
pub mod sender_allocation;
pub mod sender_fee_tracker;
pub mod unaggregated_receipts;

pub async fn start_agent() -> (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>) {
    let Config {
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
                network_subgraph_auth_token,
                allocation_syncing_interval_ms,
                recently_closed_allocation_buffer_seconds,
            },
        escrow_subgraph:
            EscrowSubgraph {
                escrow_subgraph_deployment,
                escrow_subgraph_endpoint,
                escrow_subgraph_auth_token,
                escrow_syncing_interval_ms,
            },
        tap:
            Tap {
                // TODO: replace with a proper implementation once the gateway registry contract is ready
                sender_aggregator_endpoints,
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
        DeploymentDetails::for_query_url(
            network_subgraph_endpoint,
            network_subgraph_auth_token.clone(),
        )
        .expect("Failed to parse network subgraph endpoint"),
    )));

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        *indexer_address,
        Duration::from_millis(*allocation_syncing_interval_ms),
        Duration::from_secs(*recently_closed_allocation_buffer_seconds),
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
        DeploymentDetails::for_query_url(
            escrow_subgraph_endpoint,
            escrow_subgraph_auth_token.clone(),
        )
        .expect("Failed to parse escrow subgraph endpoint"),
    )));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        *indexer_address,
        Duration::from_millis(*escrow_syncing_interval_ms),
        false,
    );

    let args = SenderAccountsManagerArgs {
        config: &CONFIG,
        domain_separator: EIP_712_DOMAIN.clone(),
        pgpool,
        indexer_allocations,
        escrow_accounts,
        escrow_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: None,
    };

    SenderAccountsManager::spawn(None, SenderAccountsManager, args)
        .await
        .expect("Failed to start sender accounts manager actor.")
}
