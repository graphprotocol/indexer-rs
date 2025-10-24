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
use indexer_monitor::{escrow_accounts_v1, escrow_accounts_v2, DeploymentDetails, SubgraphClient};
use release::IndexerServiceRelease;
use reqwest::Url;
use tap_core::tap_eip712_domain;
use tokio::{net::TcpListener, signal};
use tower_http::normalize_path::NormalizePath;
use tracing::info;

use crate::{cli::Cli, database, metrics::serve_metrics};

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

const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

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

    let http_client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .timeout(HTTP_CLIENT_TIMEOUT)
        .build()
        .expect("Failed to init HTTP client");

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

    let domain_separator = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        config.blockchain.receipts_verifier_address,
        tap_core::TapVersion::V1,
    );

    let domain_separator_v2 = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        if config.tap_mode().is_horizon() {
            config
                .blockchain
                .receipts_verifier_address_v2
                .expect("receipts_verifier_address_v2 is required when Horizon mode is enabled ([horizon].enabled = true)")
        } else {
            config
                .blockchain
                .receipts_verifier_address_v2
                .unwrap_or(config.blockchain.receipts_verifier_address)
        },
        tap_core::TapVersion::V2,
    );
    let chain_id = config.blockchain.chain_id as u64;

    let host_and_port = config.service.host_and_port;
    let indexer_address = config.indexer.indexer_address;
    let ipfs_url = config.service.ipfs_url.clone();

    // Capture individual fields needed for DIPS before they get moved
    let escrow_v1_query_url_for_dips = config.subgraphs.escrow.config.query_url.clone();
    // V2 escrow accounts are in the network subgraph
    let escrow_v2_query_url_for_dips = Some(config.subgraphs.network.config.query_url.clone());

    // Determine if we should check for Horizon contracts and potentially enable hybrid mode:
    // - Legacy mode: if [horizon].enabled = false
    // - Horizon mode: if [horizon].enabled = true; verify network readiness
    let is_horizon_active = if config.tap_mode().is_horizon() {
        tracing::info!(
            "Horizon mode configured - checking if Horizon contracts are active in the network"
        );
        match indexer_monitor::is_horizon_active(network_subgraph).await {
            Ok(true) => {
                tracing::info!("Horizon contracts detected in network subgraph - enabling hybrid migration mode");
                tracing::info!("Mode: Accept new V2 receipts only, continue processing existing V1 receipts for RAVs");
                true
            }
            Ok(false) => {
                anyhow::bail!(
                    "Horizon enabled, but the Network Subgraph indicates Horizon is not active (no PaymentsEscrow accounts found). \
                    Deploy Horizon (V2) contracts and the updated Network Subgraph, or disable Horizon ([horizon].enabled = false)"
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
    } else {
        tracing::info!("Horizon not configured - using pure legacy mode");
        false
    };

    // Configure router with escrow watchers based on automatic Horizon detection
    let router = if is_horizon_active {
        tracing::info!("Horizon contracts detected - using Horizon migration mode: V2 receipts only, but processing existing V1 receipts");

        // V1_LEGACY: Out of scope for Horizon security audit - Create V1 escrow watcher for processing existing receipts
        let escrow_subgraph_v1 = create_subgraph_client(
            http_client.clone(),
            &config.graph_node,
            &config.subgraphs.escrow.config,
        )
        .await;

        let v1_watcher = indexer_monitor::escrow_accounts_v1(
            escrow_subgraph_v1,
            indexer_address,
            config.subgraphs.escrow.config.syncing_interval_secs,
            true, // Reject thawing signers eagerly
        )
        .await
        .with_context(|| "Error creating escrow_accounts_v1 channel")?;

        // Create V2 escrow watcher for new receipts (V2 escrow accounts are in the network subgraph)
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

        ServiceRouter::builder()
            .database(database.clone())
            .domain_separator(domain_separator.clone())
            .domain_separator_v2(domain_separator_v2.clone())
            .graph_node(config.graph_node)
            .http_client(http_client)
            .release(release)
            .indexer(config.indexer)
            .service(config.service)
            .blockchain(config.blockchain)
            .timestamp_buffer_secs(config.tap.rav_request.timestamp_buffer_secs)
            .network_subgraph(network_subgraph, config.subgraphs.network)
            .escrow_accounts_v1(v1_watcher)
            .escrow_accounts_v2(v2_watcher)
            .build()
    } else {
        tracing::info!(
            "No Horizon contracts detected - using Legacy (V1) mode with escrow accounts v1 only"
        );
        // V1_LEGACY: Out of scope for Horizon security audit - Only create v1 watcher for legacy mode
        let escrow_subgraph_v1 = create_subgraph_client(
            http_client.clone(),
            &config.graph_node,
            &config.subgraphs.escrow.config,
        )
        .await;

        let v1_watcher = indexer_monitor::escrow_accounts_v1(
            escrow_subgraph_v1,
            indexer_address,
            config.subgraphs.escrow.config.syncing_interval_secs,
            true, // Reject thawing signers eagerly
        )
        .await
        .with_context(|| "Error creating escrow_accounts_v1 channel")?;

        ServiceRouter::builder()
            .database(database.clone())
            .domain_separator(domain_separator.clone())
            .domain_separator_v2(domain_separator_v2.clone())
            .graph_node(config.graph_node)
            .http_client(http_client)
            .release(release)
            .indexer(config.indexer)
            .service(config.service)
            .blockchain(config.blockchain)
            .timestamp_buffer_secs(config.tap.rav_request.timestamp_buffer_secs)
            .network_subgraph(network_subgraph, config.subgraphs.network)
            .escrow_accounts_v1(v1_watcher)
            .build()
    };

    serve_metrics(config.metrics.get_socket_addr());

    tracing::info!(
        address = %host_and_port,
        "Serving requests",
    );

    // AUDIT: dips does not work in hybrid mode
    // either V1 or V2 only
    if let Some(dips) = config.dips.as_ref() {
        let DipsConfig {
            host,
            port,
            allowed_payers,
            price_per_entity,
            price_per_epoch,
            additional_networks,
        } = dips;

        let addr = format!("{host}:{port}")
            .parse()
            .expect("invalid dips host port");

        let ipfs_fetcher: Arc<dyn IpfsFetcher> =
            Arc::new(IpfsClient::new(ipfs_url.as_str()).unwrap());

        // TODO: Try to re-use the same watcher for both DIPS and TAP
        // DIPS is part of Horizon/v2, so use v2 escrow watcher when available
        let dips_http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("Failed to init HTTP client");

        let escrow_subgraph_for_dips = if let Some(ref escrow_v2_url) = escrow_v2_query_url_for_dips
        {
            tracing::info!("DIPS using v2 escrow subgraph");
            // Create subgraph client for v2
            Box::leak(Box::new(
                SubgraphClient::new(
                    dips_http_client,
                    None, // No local deployment
                    DeploymentDetails::for_query_url_with_token(
                        escrow_v2_url.clone(),
                        None, // No auth token
                    ),
                )
                .await,
            ))
        } else {
            tracing::info!("DIPS falling back to v1 escrow subgraph");
            // V1_LEGACY: Out of scope for Horizon security audit - Create subgraph client for v1
            Box::leak(Box::new(
                SubgraphClient::new(
                    dips_http_client,
                    None, // No local deployment
                    DeploymentDetails::for_query_url_with_token(
                        escrow_v1_query_url_for_dips,
                        None, // No auth token
                    ),
                )
                .await,
            ))
        };

        let watcher = if escrow_v2_query_url_for_dips.is_some() {
            // Use v2 watcher for DIPS when v2 is available
            escrow_accounts_v2(
                escrow_subgraph_for_dips,
                indexer_address,
                Duration::from_secs(500),
                true,
            )
            .await
            .with_context(|| "Failed to create escrow accounts v2 watcher for DIPS")?
        } else {
            // V1_LEGACY: Out of scope for Horizon security audit - Fall back to v1 watcher
            escrow_accounts_v1(
                escrow_subgraph_for_dips,
                indexer_address,
                Duration::from_secs(500),
                true,
            )
            .await
            .with_context(|| "Failed to create escrow accounts v1 watcher for DIPS")?
        };

        let registry = NetworksRegistry::from_latest_version().await.unwrap();

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

        tokio::spawn(async move {
            info!(address = %addr, "Starting DIPS gRPC server");

            start_dips_server(addr, dips).await;
        });
    }

    let listener = TcpListener::bind(&host_and_port)
        .await
        .expect("Failed to bind to indexer-service port");

    let app = router.create_router().await?;
    let router = NormalizePath::trim_trailing_slash(app);
    //
    let service = ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(router);
    Ok(serve(listener, service)
        .with_graceful_shutdown(shutdown_handler())
        .await?)
}
async fn start_dips_server(addr: SocketAddr, service: impl IndexerDipsService) {
    tonic::transport::Server::builder()
        .add_service(IndexerDipsServiceServer::new(service))
        .serve(addr)
        .await
        .expect("unable to start dips grpc");
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

/// Graceful shutdown handler
async fn shutdown_handler() {
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
}
