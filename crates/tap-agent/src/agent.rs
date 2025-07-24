// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # agent
//!
//! The agent is a set of 3 actors:
//! - [sender_accounts_manager::SenderAccountsManager]
//! - [sender_account::SenderAccount]
//! - [sender_allocation::SenderAllocation]
//!
//! They run under a supervision tree and it goes like the following:
//!
//! [sender_accounts_manager::SenderAccountsManager] monitors allocations provided
//! by the subgraph via a [Watcher](::indexer_watcher). Every time it detects a
//! new escrow account created, it automatically spawns a [sender_account::SenderAccount].
//!
//! Manager is also responsible for spawning an pgnotify task that monitors new receipts.
//!
//! [sender_account::SenderAccount] is then responsible for keeping track of all fees
//! distributed across different allocations and also spawning [sender_allocation::SenderAllocation]s
//! that are going to process receipts and RAV requests.
//!
//! [sender_allocation::SenderAllocation] receives notifications from the spawned task and then
//! it updates its state an notifies its parent actor.
//!
//! Once [sender_account::SenderAccount] gets enough receipts, it uses its tracker to decide
//! what is the allocation with the most amount of fees and send a message to trigger a RavRequest.
//!
//! When the allocation is closed by the indexer, [sender_allocation::SenderAllocation] is
//! responsible for triggering the last rav, that will flush all pending receipts and mark the rav
//! as last to be redeemed by indexer-agent.
//!
//! ## Actors
//! Actors are implemented using the [ractor] library and contain their own message queue.
//! They process one message at a time and that's why concurrent primitives like
//! [std::sync::Mutex]s aren't needed.

use indexer_config::{
    Config, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    SubgraphConfig, SubgraphsConfig, TapConfig,
};
use indexer_monitor::{
    empty_escrow_accounts_watcher, escrow_accounts_v1, escrow_accounts_v2, indexer_allocations,
    DeploymentDetails, SubgraphClient,
};
use ractor::concurrency::JoinHandle;
use sender_account::SenderAccountConfig;
use sender_accounts_manager_task::SenderAccountsManagerTask;

use crate::{
    actor_migrate::LifecycleManager,
    agent::sender_accounts_manager::{SenderAccountsManagerArgs, SenderAccountsManagerMessage},
    database, CONFIG, EIP_712_DOMAIN,
};

/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_account::SenderAccount]
pub mod sender_account;
/// Tokio task-based replacement for SenderAccount actor
pub mod sender_account_task;
/// Actor, Arguments, State, Messages and implementation for
/// [crate::agent::sender_accounts_manager::SenderAccountsManager]
pub mod sender_accounts_manager;
/// Tokio task-based replacement for SenderAccountsManager actor
pub mod sender_accounts_manager_task;
/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_allocation::SenderAllocation]
pub mod sender_allocation;
/// Tokio task-based replacement for SenderAllocation actor
pub mod sender_allocation_task;
/// Unaggregated receipts containing total value and last id stored in the table
pub mod unaggregated_receipts;

use crate::actor_migrate::TaskHandle;

/// This is the main entrypoint for starting up tap-agent
///
/// It uses the static [crate::CONFIG] to configure the agent.
///
/// 🎯 TOKIO MIGRATION: Now returns TaskHandle instead of ActorRef
pub async fn start_agent() -> (TaskHandle<SenderAccountsManagerMessage>, JoinHandle<()>) {
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

    let escrow_accounts_v1 = escrow_accounts_v1(
        escrow_subgraph,
        *indexer_address,
        *escrow_sync_interval,
        false,
    )
    .await
    .expect("Error creating escrow_accounts channel");

    // Determine if we should check for Horizon contracts and potentially enable hybrid mode:
    // - If horizon.enabled = false: Pure legacy mode, no Horizon detection
    // - If horizon.enabled = true: Check if Horizon contracts are active in the network
    let is_horizon_enabled = if CONFIG.horizon.enabled {
        tracing::info!("Horizon migration support enabled - checking if Horizon contracts are active in the network");
        match indexer_monitor::is_horizon_active(network_subgraph).await {
            Ok(active) => {
                if active {
                    tracing::info!("Horizon contracts detected in network subgraph - enabling hybrid migration mode");
                    tracing::info!("TAP Agent Mode: Process existing V1 receipts for RAVs, accept new V2 receipts");
                } else {
                    tracing::info!("Horizon contracts not yet active in network subgraph - remaining in legacy mode");
                }
                active
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to detect Horizon contracts: {}. Remaining in legacy mode.",
                    e
                );
                false
            }
        }
    } else {
        tracing::info!(
            "Horizon migration support disabled in configuration - using pure legacy mode"
        );
        false
    };

    // Create V2 escrow accounts watcher only if Horizon is active
    // V2 escrow accounts are in the network subgraph, not a separate TAP v2 subgraph
    let escrow_accounts_v2 = if is_horizon_enabled {
        escrow_accounts_v2(
            network_subgraph,
            *indexer_address,
            *network_sync_interval,
            false,
        )
        .await
        .expect("Error creating escrow_accounts_v2 channel")
    } else {
        // Create a dummy watcher that never updates for consistency
        empty_escrow_accounts_watcher()
    };

    // In both modes we need both watchers for the hybrid processing
    let (escrow_accounts_v1_final, escrow_accounts_v2_final) = if is_horizon_enabled {
        tracing::info!("TAP Agent: Horizon migration mode - processing existing V1 receipts and new V2 receipts");
        (escrow_accounts_v1, escrow_accounts_v2)
    } else {
        tracing::info!("TAP Agent: Legacy mode - V1 receipts only");
        (escrow_accounts_v1, escrow_accounts_v2)
    };

    let config = Box::leak(Box::new({
        let mut config = SenderAccountConfig::from_config(&CONFIG);
        config.horizon_enabled = is_horizon_enabled;
        config
    }));

    let _args = SenderAccountsManagerArgs {
        config,
        domain_separator: EIP_712_DOMAIN.clone(),
        pgpool: pgpool.clone(),
        indexer_allocations,
        escrow_accounts_v1: escrow_accounts_v1_final,
        escrow_accounts_v2: escrow_accounts_v2_final,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: None,
    };

    // 🎯 TOKIO MIGRATION: Using SenderAccountsManagerTask instead of ractor SenderAccountsManager
    let lifecycle = LifecycleManager::new();

    let task_handle = SenderAccountsManagerTask::spawn(
        &lifecycle,
        None, // name
        config,
        pgpool,
        escrow_subgraph,
        network_subgraph,
        EIP_712_DOMAIN.clone(),
        sender_aggregator_endpoints.clone(),
        None, // prefix
    )
    .await
    .expect("Failed to start sender accounts manager task.");

    // Create a dummy JoinHandle for compatibility with main.rs
    // In the tokio model, the lifecycle manager handles task monitoring
    let dummy_handle = tokio::spawn(async {
        // This task runs indefinitely until the application shuts down
        // The actual work is done by the SenderAccountsManagerTask and its children
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });

    (task_handle, dummy_handle)
}
