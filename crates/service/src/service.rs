// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, time::Duration};

use anyhow::anyhow;
use axum::{extract::Request, serve, ServiceExt};
use clap::Parser;
use indexer_config::{Config, GraphNodeConfig, SubgraphConfig};
use indexer_monitor::{DeploymentDetails, SubgraphClient};
use release::IndexerServiceRelease;
use reqwest::Url;
use tap_core::tap_eip712_domain;
use tokio::{net::TcpListener, signal};
use tower_http::normalize_path::NormalizePath;

use crate::{cli::Cli, database, metrics::serve_metrics};

mod release;
mod router;
mod tap_receipt_header;

pub use router::ServiceRouter;
pub use tap_receipt_header::TapReceipt;

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
                "Invalid configuration file `{}`: {}, if a value is missing you can also use \
                --config to fill the rest of the values",
                cli.config.unwrap_or_default().display(),
                e
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

    let escrow_subgraph = create_subgraph_client(
        http_client.clone(),
        &config.graph_node,
        &config.subgraphs.escrow.config,
    )
    .await;

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
    );

    let host_and_port = config.service.host_and_port;

    let router = ServiceRouter::builder()
        .database(database)
        .domain_separator(domain_separator)
        .graph_node(config.graph_node)
        .http_client(http_client)
        .release(release)
        .indexer(config.indexer)
        .service(config.service)
        .dips(config.dips)
        .blockchain(config.blockchain)
        .timestamp_buffer_secs(config.tap.rav_request.timestamp_buffer_secs)
        .network_subgraph(network_subgraph, config.subgraphs.network)
        .escrow_subgraph(escrow_subgraph, config.subgraphs.escrow)
        .build();

    serve_metrics(config.metrics.get_socket_addr());

    tracing::info!(
        address = %host_and_port,
        "Serving requests",
    );
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
