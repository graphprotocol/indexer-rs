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
use indexer_monitor::{escrow_accounts_v2, indexer_allocations, DeploymentDetails, SubgraphClient};
use ractor::{concurrency::JoinHandle, Actor, ActorRef};
use sender_account::SenderAccountConfig;
use sender_accounts_manager::SenderAccountsManager;

use crate::{
    agent::sender_accounts_manager::{SenderAccountsManagerArgs, SenderAccountsManagerMessage},
    database, CONFIG, EIP_712_DOMAIN_V2,
};

/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_account::SenderAccount]
pub mod sender_account;
/// Actor, Arguments, State, Messages and implementation for
/// [crate::agent::sender_accounts_manager::SenderAccountsManager]
pub mod sender_accounts_manager;
/// Actor, Arguments, State, Messages and implementation for [crate::agent::sender_allocation::SenderAllocation]
pub mod sender_allocation;
/// Unaggregated receipts containing total value and last id stored in the table
pub mod unaggregated_receipts;

/// Force initialization of all Prometheus metrics in the agent module.
///
/// This should be called at startup before the metrics server is started
/// to ensure all metrics are registered with Prometheus, even if no sender
/// activity has occurred yet.
pub fn init_metrics() {
    sender_account::init_metrics();
    sender_allocation::init_metrics();
    sender_accounts_manager::init_metrics();
}

/// This is the main entrypoint for starting up tap-agent
///
/// It uses the static [crate::CONFIG] to configure the agent.
pub async fn start_agent(
) -> anyhow::Result<(ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>)> {
    use anyhow::Context;

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
                        max_data_staleness_mins,
                    },
                escrow: EscrowSubgraphConfig { .. },
            },
        tap: TapConfig {
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
        *max_data_staleness_mins,
    )
    .await
    .with_context(|| "Failed to initialize indexer_allocations watcher")?;

    // Verify Horizon mode is enabled and the network subgraph is ready
    if !CONFIG.tap_mode().is_horizon() {
        anyhow::bail!("Legacy TAP mode is no longer supported; enable Horizon mode");
    }

    tracing::info!("Horizon mode configured; checking network subgraph readiness");
    match indexer_monitor::is_horizon_active(network_subgraph).await {
        Ok(true) => {
            tracing::info!("Horizon schema available in network subgraph - enabling Horizon mode");
            tracing::info!(
                "V2 watcher will automatically detect new PaymentsEscrow accounts as they appear"
            );
        }
        Ok(false) => {
            anyhow::bail!(
                "Horizon mode is required, but the Network Subgraph indicates Horizon is not active (no PaymentsEscrow accounts found). \
                Ensure Horizon contracts are deployed and the Network Subgraph is updated before starting the TAP agent."
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to detect Horizon contracts due to network/subgraph error: {}. Cannot start with Horizon enabled when network status is unknown.",
                e
            );
        }
    }

    // Create V2 escrow accounts watcher
    // V2 escrow accounts are in the network subgraph, not a separate TAP v2 subgraph
    tracing::info!(
        "Initializing V2 escrow accounts watcher with indexer {}",
        indexer_address
    );
    let escrow_accounts_v2 = escrow_accounts_v2(
        network_subgraph,
        *indexer_address,
        *network_sync_interval,
        false,
    )
    .await
    .with_context(|| "Error creating escrow_accounts_v2 channel")?;

    let config = Box::leak(Box::new(SenderAccountConfig::from_config(&CONFIG)));

    let args = SenderAccountsManagerArgs {
        config,
        domain_separator_v2: EIP_712_DOMAIN_V2.clone(),
        pgpool,
        indexer_allocations,
        escrow_accounts_v2,
        network_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: None,
    };

    Ok(SenderAccountsManager::spawn(None, SenderAccountsManager, args).await?)
}

#[cfg(test)]
mod tests {
    use prometheus::core::Collector;

    use super::*;

    #[test]
    fn test_init_metrics_registers_all_metrics() {
        // Call init_metrics to register all metrics
        init_metrics();

        // Verify that calling init_metrics doesn't panic (metrics are already registered)
        // This is the main test - that we can safely call init_metrics at startup.
        // The LazyLock pattern ensures metrics are only registered once.
        init_metrics();

        // Verify metrics are registered by directly accessing the statics.
        // This ensures the LazyLock has been initialized.
        // We use desc() to get the metric descriptors which proves they're registered.

        // Check sender_account metrics
        assert!(
            !sender_account::SENDER_DENIED.desc().is_empty(),
            "tap_sender_denied should be registered"
        );
        assert!(
            !sender_account::ESCROW_BALANCE.desc().is_empty(),
            "tap_sender_escrow_balance_grt_total should be registered"
        );
        assert!(
            !sender_account::UNAGGREGATED_FEES_BY_VERSION
                .desc()
                .is_empty(),
            "tap_unaggregated_fees_grt_total_by_version should be registered"
        );
        assert!(
            !sender_account::SENDER_FEE_TRACKER.desc().is_empty(),
            "tap_sender_fee_tracker_grt_total should be registered"
        );
        assert!(
            !sender_account::INVALID_RECEIPT_FEES.desc().is_empty(),
            "tap_invalid_receipt_fees_grt_total should be registered"
        );
        assert!(
            !sender_account::PENDING_RAV.desc().is_empty(),
            "tap_pending_rav_grt_total should be registered"
        );
        assert!(
            !sender_account::MAX_FEE_PER_SENDER.desc().is_empty(),
            "tap_max_fee_per_sender_grt_total should be registered"
        );
        assert!(
            !sender_account::RAV_REQUEST_TRIGGER_VALUE.desc().is_empty(),
            "tap_rav_request_trigger_value should be registered"
        );
        assert!(
            !sender_account::ALLOCATION_RECONCILIATION_RUNS
                .desc()
                .is_empty(),
            "tap_allocation_reconciliation_runs_total should be registered"
        );

        // Check sender_allocation metrics
        assert!(
            !sender_allocation::CLOSED_SENDER_ALLOCATIONS
                .desc()
                .is_empty(),
            "tap_closed_sender_allocation_total should be registered"
        );
        assert!(
            !sender_allocation::RAVS_CREATED_BY_VERSION.desc().is_empty(),
            "tap_ravs_created_total_by_version should be registered"
        );
        assert!(
            !sender_allocation::RAVS_FAILED_BY_VERSION.desc().is_empty(),
            "tap_ravs_failed_total_by_version should be registered"
        );
        assert!(
            !sender_allocation::RAV_RESPONSE_TIME_BY_VERSION
                .desc()
                .is_empty(),
            "tap_rav_response_time_seconds_by_version should be registered"
        );

        // Check sender_accounts_manager metrics
        assert!(
            !sender_accounts_manager::RECEIPTS_CREATED.desc().is_empty(),
            "tap_receipts_received_total should be registered"
        );
    }
}
