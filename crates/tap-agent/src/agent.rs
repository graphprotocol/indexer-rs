// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # TAP Agent
//!
//! The TAP (Timeline Aggregation Protocol) Agent is a service that processes micropayment
//! receipts from gateways and aggregates them into Receipt Aggregate Vouchers (RAVs) for
//! efficient on-chain settlement.
//!
//! ## Architecture Overview
//!
//! The agent uses a stream-based processing architecture built on tokio with the following
//! key components:
//!
//! ### Stream Processing Pipeline
//!
//! 1. **PostgreSQL Event Source**: Listens for database notifications when new receipts arrive
//! 2. **Validation Service**: Verifies receipt signatures and checks sender account balances  
//! 3. **Processing Pipeline**: Aggregates receipts by allocation until thresholds are met
//! 4. **RAV Creation**: Coordinates with sender aggregator services to sign RAVs
//! 5. **Persistence Service**: Stores completed RAVs in database for indexer-agent redemption
//!
//! ### Dual Protocol Support
//!
//! The agent supports both Legacy (v1) and Horizon (v2) TAP protocols:
//! - **Legacy**: Uses 20-byte allocation IDs and separate TAP subgraph
//! - **Horizon**: Uses 32-byte collection IDs integrated with network subgraph
//!
//! The agent automatically detects which protocol version is active and processes
//! both types during migration periods.

use bigdecimal::ToPrimitive;
use indexer_config::{
    Config, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    SubgraphConfig, SubgraphsConfig, TapConfig,
};
use indexer_monitor::{DeploymentDetails, SubgraphClient};

use crate::database;

// Import stream processor for production implementation
use crate::agent::tap_agent::{run_tap_agent, TapAgentConfig};

/// AllocationId wrapper enum for dual Legacy/Horizon support
pub mod allocation_id;
/// PostgreSQL event source for stream processing
pub mod postgres_source;
/// Stream-based TAP processing pipeline
pub mod stream_processor;
/// Actor, Arguments, State, Messages and implementation for [crate::agent::tap_agent::TapAgent]
pub mod tap_agent;
/// Unaggregated receipts containing total value and last id stored in the table
pub mod unaggregated_receipts;

/// Start the TAP Agent with stream-based processing architecture
///
/// This function initializes and runs the complete TAP (Timeline Aggregation Protocol)
/// processing system that handles micropayment receipts from gateways and aggregates
/// them into Receipt Aggregate Vouchers (RAVs) for on-chain redemption.
///
/// ## Processing Pipeline
///
/// The TAP Agent processes receipts through the following pipeline:
/// 1. **Receipt Ingestion**: PostgreSQL notifications trigger processing of new receipts
/// 2. **Validation**: Receipts are validated using EIP-712 signature verification
/// 3. **Aggregation**: Valid receipts are aggregated per allocation until threshold is reached
/// 4. **RAV Creation**: Aggregated receipts are signed by sender's aggregator service
/// 5. **Persistence**: Completed RAVs are stored for indexer-agent to redeem on-chain
///
/// ## Key Features
///
/// - **Dual Protocol Support**: Handles both Legacy (v1) and Horizon (v2) TAP protocols
/// - **Real-time Processing**: Event-driven architecture using PostgreSQL LISTEN/NOTIFY
/// - **Escrow Protection**: Monitors sender account balances to prevent overdrafts
/// - **Graceful Shutdown**: Cleanly completes in-flight work before termination
/// - **High Throughput**: Stream-based design with configurable buffer sizes
///
/// **Production Entry Point**: Uses global configuration for production deployment.
/// For testing with dependency injection, use `start_stream_based_agent_with_config()`.
pub async fn start_stream_based_agent() -> anyhow::Result<()> {
    use crate::{CONFIG, EIP_712_DOMAIN};
    start_stream_based_agent_with_config(&CONFIG, &EIP_712_DOMAIN).await
}

/// Start the TAP Agent with dependency-injected configuration
///
/// This is the core implementation that accepts configuration for proper
/// dependency injection, enabling testability and avoiding the global config antipattern.
///
/// ## Architecture Benefits
///
/// - **Testability**: Tests can inject complete test configurations
/// - **Isolation**: No dependency on global configuration statics
/// - **Functional Design**: Pure function with explicit dependencies
/// - **Production Compatible**: Uses exact same code path as production
///
/// ## Parameters
///
/// - `config`: Complete indexer configuration (database, subgraphs, TAP settings)
/// - `eip712_domain`: EIP-712 domain for receipt signature verification
pub async fn start_stream_based_agent_with_config(
    config: &Config,
    eip712_domain: &thegraph_core::alloy::sol_types::Eip712Domain,
) -> anyhow::Result<()> {
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
        tap: TapConfig {
            sender_aggregator_endpoints,
            ..
        },
        ..
    } = config;

    // Connect to database using provided configuration
    let pgpool = database::connect(database.clone()).await;
    let http_client = reqwest::Client::new();

    // Create network subgraph client
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

    // Create escrow subgraph client
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

    // Determine Horizon support
    let is_horizon_enabled = if config.horizon.enabled {
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

    // Create TapAgentConfig for stream processor
    // Calculate RAV threshold from max_amount_willing_to_lose_grt and trigger_value_divisor
    let max_willing_to_lose = config.tap.max_amount_willing_to_lose_grt.get_value(); // Already in wei (u128)
    let trigger_divisor = config
        .tap
        .rav_request
        .trigger_value_divisor
        .to_u128()
        .unwrap_or(10u128);
    let rav_threshold = max_willing_to_lose / trigger_divisor;

    let tap_config = TapAgentConfig {
        pgpool,
        rav_threshold,
        rav_request_interval: config.tap.rav_request.timestamp_buffer_secs,
        event_buffer_size: 1000,
        result_buffer_size: 1000,
        rav_buffer_size: 100,

        // Escrow configuration
        escrow_subgraph_v1: Some(escrow_subgraph),
        escrow_subgraph_v2: if is_horizon_enabled {
            Some(network_subgraph)
        } else {
            None
        },
        indexer_address: config.indexer.indexer_address,
        escrow_syncing_interval: std::time::Duration::from_secs(60),
        reject_thawing_signers: true,

        // Network subgraph configuration
        network_subgraph: Some(network_subgraph),
        allocation_syncing_interval: std::time::Duration::from_secs(120),
        recently_closed_allocation_buffer: std::time::Duration::from_secs(300),

        // TAP Manager configuration
        domain_separator: Some(eip712_domain.clone()),
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
    };

    tracing::info!("🚀 Starting stream-based TAP agent (production implementation)");
    tracing::info!("📊 Configuration:");
    tracing::info!("  • RAV threshold: {}", tap_config.rav_threshold);
    tracing::info!(
        "  • RAV request interval: {:?}",
        tap_config.rav_request_interval
    );
    tracing::info!("  • Horizon enabled: {}", is_horizon_enabled);
    tracing::info!(
        "  • Aggregator endpoints: {}",
        tap_config.sender_aggregator_endpoints.len()
    );

    // Run the stream-based TAP agent
    run_tap_agent(tap_config).await
}
