// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_config::{
    Config, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    SubgraphConfig, SubgraphsConfig, TapConfig,
};
use indexer_monitor::{escrow_accounts, indexer_allocations, DeploymentDetails, SubgraphClient};
use ractor::{concurrency::JoinHandle, Actor, ActorRef};
use sender_account::SenderAccountConfig;
use sender_accounts_manager::SenderAccountsManager;

use crate::{
    agent::sender_accounts_manager::{SenderAccountsManagerArgs, SenderAccountsManagerMessage},
    database, CONFIG, EIP_712_DOMAIN,
};

pub mod sender_account;
pub mod sender_accounts_manager;
pub mod sender_allocation;
pub mod unaggregated_receipts;

pub async fn start_agent() -> (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>) {
    let Config {
        indexer: IndexerConfig {
            indexer_address, ..
        },
        graph_node:
            GraphNodeConfig {
                status_url: graph_node_status_endpoint,
                query_url: graph_node_query_endpoint,
            },
        database,
        subgraphs:
            SubgraphsConfig {
                network:
                    NetworkSubgraphConfig {
                        config:
                            SubgraphConfig {
                                query_url: network_query_url,
                                query_auth_token: network_query_auth_token,
                                deployment_id: network_deployment_id,
                                syncing_interval_secs: network_sync_interval,
                            },
                        recently_closed_allocation_buffer_secs: recently_closed_allocation_buffer,
                    },
                escrow:
                    EscrowSubgraphConfig {
                        config:
                            SubgraphConfig {
                                query_url: escrow_query_url,
                                query_auth_token: escrow_query_auth_token,
                                deployment_id: escrow_deployment_id,
                                syncing_interval_secs: escrow_sync_interval,
                            },
                    },
            },
        tap:
            TapConfig {
                // TODO: replace with a proper implementation once the gateway registry contract is ready
                sender_aggregator_endpoints,
                ..
            },
        ..
    } = &*CONFIG;
    let pgpool = database::connect(database.clone()).await;

    let http_client = reqwest::Client::new();

    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            network_deployment_id.map(|deployment| {
                DeploymentDetails::for_graph_node_url(
                    graph_node_status_endpoint.clone(),
                    graph_node_query_endpoint.clone(),
                    deployment,
                )
            }),
            DeploymentDetails::for_query_url_with_token(
                network_query_url.clone(),
                network_query_auth_token.clone(),
            ),
        )
        .await,
    ));

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        *indexer_address,
        *network_sync_interval,
        *recently_closed_allocation_buffer,
    )
    .await
    .expect("Failed to initialize indexer_allocations watcher");

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            escrow_deployment_id.map(|deployment| {
                DeploymentDetails::for_graph_node_url(
                    graph_node_status_endpoint.clone(),
                    graph_node_query_endpoint.clone(),
                    deployment,
                )
            }),
            DeploymentDetails::for_query_url_with_token(
                escrow_query_url.clone(),
                escrow_query_auth_token.clone(),
            ),
        )
        .await,
    ));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        *indexer_address,
        *escrow_sync_interval,
        false,
    )
    .await
    .expect("Error creating escrow_accounts channel");

    let config = Box::leak(Box::new(SenderAccountConfig::from_config(&CONFIG)));

    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: EIP_712_DOMAIN.clone(),
        pgpool,
        indexer_allocations,
        escrow_accounts,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: None,
    };

    SenderAccountsManager::spawn(None, SenderAccountsManager, args)
        .await
        .expect("Failed to start sender accounts manager actor.")
}
