// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

pub mod sender;

use std::time::Duration;

use indexer_common::prelude::{
    escrow_accounts, indexer_allocations, DeploymentDetails, SubgraphClient,
};
use ractor::{concurrency::JoinHandle, Actor, ActorRef};

use crate::{
    config::{Config, EscrowSubgraph, Ethereum, IndexerInfrastructure, NetworkSubgraph, Tap},
    database, CONFIG, EIP_712_DOMAIN,
};
use sender::accounts_manager::{
    SenderAccountsManager, SenderAccountsManagerArgs, SenderAccountsManagerMessage,
};

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct UnaggregatedReceipts {
    pub value: u128,
    /// The ID of the last receipt value added to the unaggregated fees value.
    /// This is used to make sure we don't process the same receipt twice. Relies on the fact that
    /// the receipts IDs are SERIAL in the database.
    pub last_id: u64,
}

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
        DeploymentDetails::for_query_url_with_token(
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
        DeploymentDetails::for_query_url_with_token(
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
