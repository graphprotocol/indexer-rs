// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{anyhow, Context};
use axum::{extract::Request, serve, ServiceExt};
use clap::Parser;
use graph_networks_registry::NetworksRegistry;
use indexer_config::{Config, DipsConfig, GraphNodeConfig, SubgraphConfig};
use indexer_dips::{
    database::PsqlAgreementStore,
    ipfs::{IpfsClient, IpfsFetcher},
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::indexer_dips_service_server::{
        IndexerDipsService, IndexerDipsServiceServer,
    },
    server::{DipsServer, DipsServerContext},
    signers::EscrowSignerValidator,
};
use indexer_monitor::{escrow_accounts_v2, DeploymentDetails, SubgraphClient};
use release::IndexerServiceRelease;
use reqwest::Url;
use tap_core::tap_eip712_domain;
use tokio::{net::TcpListener, signal};
use tokio_util::sync::CancellationToken;
use tower_http::normalize_path::NormalizePath;
use tracing::info;

use crate::{
    cli::Cli,
    constants::{DIPS_HTTP_CLIENT_TIMEOUT, HTTP_CLIENT_TIMEOUT},
    database,
    metrics::serve_metrics,
};

mod release;
mod router;
mod tap_receipt_header;

pub use router::ServiceRouter;
pub use tap_receipt_header::TapHeader;

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

    // V2 escrow accounts (used by DIPS) are in the network subgraph
    let escrow_v2_query_url_for_dips = config.subgraphs.network.config.query_url.clone();

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
        .escrow_accounts_v2(v2_watcher)
        .build();

    serve_metrics(config.metrics.get_socket_addr());

    // Create a cancellation token for coordinated graceful shutdown
    let shutdown_token = CancellationToken::new();

    tracing::info!(
        address = %host_and_port,
        "Serving requests",
    );
    if let Some(dips) = config.dips.as_ref() {
        let DipsConfig {
            host,
            port,
            allowed_payers,
            price_per_entity,
            price_per_epoch,
            additional_networks,
        } = dips;

        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .with_context(|| format!("Invalid DIPS host:port '{host}:{port}'"))?;

        let ipfs_fetcher: Arc<dyn IpfsFetcher> = Arc::new(
            IpfsClient::new(ipfs_url.as_str())
                .with_context(|| format!("Failed to create IPFS client for URL '{ipfs_url}'"))?,
        );

        // TODO: Try to re-use the same watcher for both DIPS and TAP
        // DIPS requires Horizon/V2, so always use V2 escrow from network subgraph
        let dips_http_client = create_http_client(DIPS_HTTP_CLIENT_TIMEOUT, false)
            .context("Failed to create DIPS HTTP client")?;

        tracing::info!("DIPS using V2 escrow from network subgraph");
        let escrow_subgraph_for_dips = Box::leak(Box::new(
            SubgraphClient::new(
                dips_http_client,
                None, // No local deployment
                DeploymentDetails::for_query_url_with_token(
                    escrow_v2_query_url_for_dips.clone(),
                    None, // No auth token
                ),
            )
            .await,
        ));

        let watcher = escrow_accounts_v2(
            escrow_subgraph_for_dips,
            indexer_address,
            Duration::from_secs(500),
            true,
        )
        .await
        .with_context(|| "Failed to create escrow accounts V2 watcher for DIPS")?;

        let registry = NetworksRegistry::from_latest_version()
            .await
            .context("Failed to fetch networks registry")?;

        let ctx = DipsServerContext {
            store: Arc::new(PsqlAgreementStore {
                pool: database.clone(),
            }),
            ipfs_fetcher,
            price_calculator: PriceCalculator::new(price_per_epoch.clone(), *price_per_entity),
            signer_validator: Arc::new(EscrowSignerValidator::new(watcher)),
            registry: Arc::new(registry),
            additional_networks: Arc::new(additional_networks.clone()),
        };

        let dips = DipsServer {
            ctx: Arc::new(ctx),
            expected_payee: indexer_address,
            allowed_payers: allowed_payers.clone(),
            chain_id,
        };

        info!(address = %addr, "Starting DIPS gRPC server");

        let dips_shutdown_token = shutdown_token.clone();
        tokio::spawn(async move {
            start_dips_server(addr, dips, dips_shutdown_token.cancelled()).await;
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
