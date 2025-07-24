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
use indexer_monitor::{DeploymentDetails, SubgraphClient};
use sender_account::SenderAccountConfig;
use sender_accounts_manager_task::SenderAccountsManagerTask;
use tokio::task::JoinHandle;

use crate::{
    agent::sender_accounts_manager::SenderAccountsManagerMessage, database,
    task_lifecycle::LifecycleManager, CONFIG, EIP_712_DOMAIN,
};

/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_account::SenderAccount]
pub mod sender_account;
/// SenderAccount task implementation
pub mod sender_account_task;
/// Actor, Arguments, State, Messages and implementation for
/// [crate::agent::sender_accounts_manager::SenderAccountsManager]
pub mod sender_accounts_manager;
/// SenderAccountsManager task implementation
pub mod sender_accounts_manager_task;
/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_allocation::SenderAllocation]
pub mod sender_allocation;
/// SenderAllocation task implementation
pub mod sender_allocation_task;
/// Tests for task lifecycle monitoring and health checks
#[cfg(test)]
mod test_lifecycle_monitoring;
/// Comprehensive integration tests
#[cfg(test)]
mod test_tokio_migration;
/// Regression tests for system behavior
#[cfg(test)]
mod test_tokio_regression;
/// Unaggregated receipts containing total value and last id stored in the table
pub mod unaggregated_receipts;

use crate::task_lifecycle::TaskHandle;

/// This is the main entrypoint for starting up tap-agent
///
/// It uses the static [crate::CONFIG] to configure the agent.
///
/// Returns:
/// - TaskHandle for the main SenderAccountsManagerTask
/// - JoinHandle for the system health monitoring task that triggers shutdown on critical failures
pub async fn start_agent() -> (TaskHandle<SenderAccountsManagerMessage>, JoinHandle<()>) {
    let Config {
        indexer: IndexerConfig { .. },
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
                                ..
                            },
                        ..
                    },
                escrow:
                    EscrowSubgraphConfig {
                        config:
                            SubgraphConfig {
                                query_url: escrow_query_url,
                                query_auth_token: escrow_query_auth_token,
                                deployment_id: escrow_deployment_id,
                                ..
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

    // Note: indexer_allocations watcher is not needed here as SenderAccountsManagerTask
    // creates its own PostgreSQL notification listeners for receipt events

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

    // Log the TAP Agent mode based on Horizon detection
    if is_horizon_enabled {
        tracing::info!("TAP Agent: Horizon migration mode - processing existing V1 receipts and new V2 receipts");
    } else {
        tracing::info!("TAP Agent: Legacy mode - V1 receipts only");
    }

    // Note: escrow_accounts watchers are not needed here as SenderAccountsManagerTask
    // handles escrow account monitoring through its own internal mechanisms

    let config = Box::leak(Box::new({
        let mut config = SenderAccountConfig::from_config(&CONFIG);
        config.horizon_enabled = is_horizon_enabled;
        config
    }));

    // Initialize the tokio-based sender accounts manager
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

    // Create a proper system monitoring task that integrates with LifecycleManager
    // This task monitors overall system health and can trigger graceful shutdown
    let monitoring_handle = tokio::spawn({
        let lifecycle_clone = lifecycle.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut consecutive_unhealthy_checks = 0;
            const MAX_UNHEALTHY_CHECKS: u32 = 12; // 60 seconds of unhealthy state

            loop {
                interval.tick().await;

                // Monitor system health
                let system_health = lifecycle_clone.get_system_health().await;

                if !system_health.overall_healthy {
                    consecutive_unhealthy_checks += 1;
                    tracing::warn!(
                        "System unhealthy: {}/{} healthy tasks, {} failed, {} restarting (check {}/{})",
                        system_health.healthy_tasks,
                        system_health.total_tasks,
                        system_health.failed_tasks,
                        system_health.restarting_tasks,
                        consecutive_unhealthy_checks,
                        MAX_UNHEALTHY_CHECKS
                    );

                    // If system has been unhealthy for too long, trigger shutdown
                    if consecutive_unhealthy_checks >= MAX_UNHEALTHY_CHECKS {
                        tracing::error!(
                            "System has been unhealthy for {} checks, initiating graceful shutdown",
                            consecutive_unhealthy_checks
                        );
                        break;
                    }
                } else {
                    // Reset counter on healthy check
                    if consecutive_unhealthy_checks > 0 {
                        tracing::info!(
                            "System health recovered: {}/{} healthy tasks",
                            system_health.healthy_tasks,
                            system_health.total_tasks
                        );
                        consecutive_unhealthy_checks = 0;
                    }
                }

                // Perform periodic health checks on all tasks
                lifecycle_clone.perform_health_check().await;
            }

            tracing::info!("System monitoring task shutting down");
        }
    });

    (task_handle, monitoring_handle)
}
