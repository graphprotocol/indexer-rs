// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{anyhow, Context};
use axum::{extract::Request, serve, ServiceExt};
use clap::Parser;
use graph_networks_registry::NetworksRegistry;
use indexer_config::{Config, DipsConfig, GraphNodeConfig, SubgraphConfig};
use indexer_dips::{
    database::PsqlRcaStore,
    ipfs::IpfsClient,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::indexer_dips_service_server::{
        IndexerDipsService, IndexerDipsServiceServer,
    },
    server::{DipsServer, DipsServerContext},
    signers::EscrowSignerValidator,
};
use indexer_monitor::{DeploymentDetails, SubgraphClient};
use release::IndexerServiceRelease;
use reqwest::Url;
use tap_core::tap_eip712_domain;
use thegraph_core::alloy::primitives::{Address, U256};
use tokio::{net::TcpListener, signal};
use tokio_util::sync::CancellationToken;
use tower_http::normalize_path::NormalizePath;
use tracing::info;

use crate::{
    cli::Cli, constants::HTTP_CLIENT_TIMEOUT, database, metrics::serve_metrics,
    routes::DipsInfoState,
};

mod release;
mod router;
mod tap_receipt_header;

pub use router::ServiceRouter;
pub use tap_receipt_header::TapHeader;

/// Format a wei value as a human-readable GRT string.
///
/// Converts wei (10^-18 GRT) to GRT with up to 18 decimal places,
/// trimming trailing zeros. For example:
/// - 1_000_000_000_000_000_000 wei -> "1"
/// - 1_500_000_000_000_000_000 wei -> "1.5"
/// - 500_000_000_000_000_000 wei -> "0.5"
fn format_grt(wei: u128) -> String {
    let whole = wei / 10u128.pow(18);
    let frac = wei % 10u128.pow(18);
    if frac == 0 {
        whole.to_string()
    } else {
        // Format with up to 18 decimal places, trimming trailing zeros
        let frac_str = format!("{:018}", frac);
        let trimmed = frac_str.trim_end_matches('0');
        format!("{}.{}", whole, trimmed)
    }
}

#[derive(Clone)]
pub struct GraphNodeState {
    pub graph_node_client: reqwest::Client,
    pub graph_node_status_url: Url,
    pub graph_node_query_base_url: Url,
}

/// Run the subgraph indexer service
pub async fn run() -> anyhow::Result<()> {
    // Parse command line and environment arguments
    let cli = Cli::parse();

    // Load the service configuration
    let config = Config::parse(indexer_config::ConfigPrefix::Service, cli.config.as_ref())
        .map_err(|e| {
            tracing::error!(
                config_path = %cli.config.unwrap_or_default().display(),
                error = %e,
                "Invalid configuration file; you can use --config to fill missing values",
            );
            anyhow!(e)
        })?;

    // Parse basic configurations
    build_info::build_info!(fn build_info);
    let release = IndexerServiceRelease::from(build_info());

    let http_client =
        create_http_client(HTTP_CLIENT_TIMEOUT, true).context("Failed to create HTTP client")?;

    let network_subgraph = create_subgraph_client(
        http_client.clone(),
        &config.graph_node,
        &config.subgraphs.network.config,
    )
    .await;

    // V2 escrow accounts are in the network subgraph, not a separate escrow_v2 subgraph

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema
    // Note: Typically, you'd call `sqlx::migrate!();` here to sync the models
    // which defaults to files in  "./migrations" to sync the database;
    // however, this can cause conflicts with the migrations run by indexer
    // agent. Hence we leave syncing and migrating entirely to the agent and
    // assume the models are up to date in the service.
    let database =
        database::connect(config.database.clone().get_formated_postgres_url().as_ref()).await;

    let domain_separator_v2 = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        config.blockchain.horizon_receipts_verifier_address(),
        tap_core::TapVersion::V2,
    );
    let chain_id = config.blockchain.chain_id as u64;

    let host_and_port = config.service.host_and_port;
    let indexer_address = config.indexer.indexer_address;
    let ipfs_url = config.service.ipfs_url.clone();

    // Horizon is required; verify contracts are active.
    if !config.tap_mode().is_horizon() {
        anyhow::bail!("Horizon mode is required; legacy mode is no longer supported.");
    }

    tracing::info!("Horizon mode configured; checking network subgraph readiness");
    match indexer_monitor::is_horizon_active(network_subgraph).await {
        Ok(true) => {
            tracing::info!("Horizon contracts detected in network subgraph");
        }
        Ok(false) => {
            anyhow::bail!(
                "Horizon mode is required, but the Network Subgraph indicates Horizon is not active (no PaymentsEscrow accounts found). \
                Ensure Horizon contracts are deployed and the Network Subgraph is updated before starting the indexer service."
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to detect Horizon contracts due to network/subgraph error: {}. \
                Cannot start with Horizon mode enabled when network status is unknown.",
                e
            );
        }
    }

    tracing::info!("Horizon contracts detected - using Horizon (V2) mode");

    let v2_watcher = match indexer_monitor::escrow_accounts_v2(
        network_subgraph,
        indexer_address,
        config.subgraphs.network.config.syncing_interval_secs,
        true, // Reject thawing signers eagerly
    )
    .await
    {
        Ok(watcher) => {
            tracing::info!("V2 escrow accounts successfully initialized from network subgraph");
            watcher
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to initialize V2 escrow accounts; service cannot continue",
            );
            std::process::exit(1);
        }
    };

    let escrow_subgraph = create_subgraph_client(
        http_client.clone(),
        &config.graph_node,
        &config.subgraphs.escrow.config,
    )
    .await;

    // Build DipsInfoState if DIPS is configured
    let dips_info_state = config.dips.as_ref().map(|dips| DipsInfoState {
        min_grt_per_30_days: dips
            .min_grt_per_30_days
            .iter()
            .map(|(network, grt)| (network.clone(), format_grt(grt.wei())))
            .collect(),
        min_grt_per_million_entities_per_30_days: format_grt(
            dips.min_grt_per_million_entities_per_30_days.wei(),
        ),
    });

    let router = ServiceRouter::builder()
        .database(database.clone())
        .domain_separator_v2(domain_separator_v2.clone())
        .graph_node(config.graph_node)
        .http_client(http_client)
        .release(release)
        .indexer(config.indexer)
        .service(config.service)
        .blockchain(config.blockchain)
        .timestamp_buffer_secs(config.tap.rav_request.timestamp_buffer_secs)
        .network_subgraph(network_subgraph, config.subgraphs.network)
        .escrow_subgraph(escrow_subgraph, config.subgraphs.escrow)
        .escrow_accounts_v2(v2_watcher.clone())
        .maybe_dips_info(dips_info_state)
        .build();

    serve_metrics(config.metrics.get_socket_addr());

    // Create a cancellation token for coordinated graceful shutdown
    let shutdown_token = CancellationToken::new();

    tracing::info!(
        address = %host_and_port,
        "Serving requests",
    );
    // DIPS: RecurringCollectionAgreement validation and storage
    if let Some(dips) = config.dips.as_ref() {
        let DipsConfig {
            host,
            port,
            recurring_collector,
            supported_networks,
            min_grt_per_30_days,
            min_grt_per_million_entities_per_30_days,
            additional_networks,
        } = dips;

        // Validate required configuration
        if *recurring_collector == Address::ZERO {
            anyhow::bail!(
                "DIPS is enabled but dips.recurring_collector is not configured. \
                 Set it to the deployed RecurringCollector contract address."
            );
        }

        if supported_networks.is_empty() {
            tracing::warn!(
                "DIPS enabled but no networks in dips.supported_networks. \
                 All proposals will be rejected."
            );
        }

        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .with_context(|| format!("Invalid DIPS host:port '{host}:{port}'"))?;

        // Initialize validation dependencies
        let ipfs_fetcher = Arc::new(IpfsClient::new(ipfs_url.as_str())?);
        let registry = Arc::new(
            NetworksRegistry::from_latest_version()
                .await
                .context("Failed to fetch NetworksRegistry for DIPS")?,
        );

        // Convert GRT/30days to wei/second for protocol compatibility.
        // Use ceiling division to protect indexers: configured minimums round UP,
        // ensuring indexers never accept less than their stated minimum.
        // 30 days = 2,592,000 seconds
        const SECONDS_PER_30_DAYS: u128 = 30 * 24 * 60 * 60;
        let tokens_per_second = min_grt_per_30_days
            .iter()
            .map(|(network, grt)| {
                let wei_per_second = grt.wei().div_ceil(SECONDS_PER_30_DAYS);
                (network.clone(), U256::from(wei_per_second))
            })
            .collect();

        // Entity pricing: config is per-million-entities, convert to per-entity.
        // Ceiling division protects indexer from precision loss.
        let entity_divisor = SECONDS_PER_30_DAYS * 1_000_000;
        let tokens_per_entity_per_second = U256::from(
            min_grt_per_million_entities_per_30_days
                .wei()
                .div_ceil(entity_divisor),
        );

        // Build server context
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(PsqlRcaStore {
                pool: database.clone(),
            }),
            ipfs_fetcher,
            price_calculator: Arc::new(PriceCalculator::new(
                supported_networks.clone(),
                tokens_per_second,
                tokens_per_entity_per_second,
            )),
            signer_validator: Arc::new(EscrowSignerValidator::new(v2_watcher.clone())),
            registry,
            additional_networks: Arc::new(additional_networks.clone()),
        });

        // Create DIPS server
        let server = DipsServer {
            ctx,
            expected_payee: indexer_address,
            chain_id,
            recurring_collector: *recurring_collector,
        };

        info!(
            address = %addr,
            recurring_collector = ?recurring_collector,
            "Starting DIPS gRPC server (RecurringCollectionAgreement validation)"
        );

        let dips_shutdown_token = shutdown_token.clone();
        tokio::spawn(async move {
            start_dips_server(addr, server, dips_shutdown_token.cancelled()).await;
        });
    }

    let listener = TcpListener::bind(&host_and_port)
        .await
        .with_context(|| format!("Failed to bind to indexer-service port '{host_and_port}'"))?;

    let app = router.create_router().await?;
    let router = NormalizePath::trim_trailing_slash(app);
    //
    let service = ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(router);
    Ok(serve(listener, service)
        .with_graceful_shutdown(shutdown_handler(shutdown_token))
        .await?)
}

async fn start_dips_server(
    addr: SocketAddr,
    service: impl IndexerDipsService,
    shutdown: impl std::future::Future<Output = ()>,
) {
    if let Err(e) = tonic::transport::Server::builder()
        .add_service(IndexerDipsServiceServer::new(service))
        .serve_with_shutdown(addr, shutdown)
        .await
    {
        tracing::error!(error = %e, "DIPS gRPC server error");
    }
}

async fn create_subgraph_client(
    http_client: reqwest::Client,
    graph_node: &GraphNodeConfig,
    subgraph_config: &SubgraphConfig,
) -> &'static SubgraphClient {
    Box::leak(Box::new(
        SubgraphClient::new(
            http_client,
            subgraph_config.deployment_id.map(|deployment| {
                DeploymentDetails::for_graph_node_url(
                    graph_node.status_url.clone(),
                    graph_node.query_url.clone(),
                    deployment,
                )
            }),
            DeploymentDetails::for_query_url_with_token(
                subgraph_config.query_url.clone(),
                subgraph_config.query_auth_token.clone(),
            ),
        )
        .await,
    ))
}

/// Creates an HTTP client with the specified timeout configuration.
///
/// # Arguments
/// * `timeout` - Maximum duration to wait for a response
/// * `tcp_nodelay` - If true, disables Nagle's algorithm for lower latency
fn create_http_client(
    timeout: Duration,
    tcp_nodelay: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .tcp_nodelay(tcp_nodelay)
        .timeout(timeout)
        .build()
}

/// Graceful shutdown handler that coordinates shutdown across all servers
async fn shutdown_handler(shutdown_token: CancellationToken) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Signal received, starting graceful shutdown");
    shutdown_token.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_grt_zero() {
        // Arrange
        let wei = 0u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0");
    }

    #[test]
    fn test_format_grt_whole_number() {
        // Arrange - 1 GRT = 10^18 wei
        let wei = 1_000_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1");
    }

    #[test]
    fn test_format_grt_large_whole_number() {
        // Arrange - 1000 GRT
        let wei = 1_000_000_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1000");
    }

    #[test]
    fn test_format_grt_small_value_less_than_one() {
        // Arrange - 0.5 GRT = 5 * 10^17 wei
        let wei = 500_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.5");
    }

    #[test]
    fn test_format_grt_very_small_value() {
        // Arrange - 0.000000000000000001 GRT = 1 wei
        let wei = 1u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.000000000000000001");
    }

    #[test]
    fn test_format_grt_mixed_value() {
        // Arrange - 1.5 GRT
        let wei = 1_500_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1.5");
    }

    #[test]
    fn test_format_grt_trims_trailing_zeros() {
        // Arrange - 1.100 GRT should become "1.1"
        let wei = 1_100_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1.1");
    }

    #[test]
    fn test_format_grt_many_decimal_places() {
        // Arrange - 0.123456789012345678 GRT
        let wei = 123_456_789_012_345_678u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.123456789012345678");
    }

    #[test]
    fn test_format_grt_large_value_with_decimals() {
        // Arrange - 12345.6789 GRT
        let wei = 12_345_678_900_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "12345.6789");
    }
}
