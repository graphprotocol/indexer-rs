// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use axum::{
    error_handling::HandleErrorLayer,
    handler::Handler,
    http::{Method, StatusCode},
    routing::get,
};
use axum::{routing::post, Extension, Router, Server};
use dotenvy::dotenv;
use ethereum_types::{Address, U256};
use model::QueryRoot;

use std::{net::SocketAddr, str::FromStr, time::Duration};
use tower::{BoxError, ServiceBuilder};
use tower_http::{
    add_extension::AddExtensionLayer,
    cors::CorsLayer,
    trace::{self, TraceLayer},
};
use tracing::{info, Level};

use util::{package_version, shutdown_signal};

use crate::{
    common::network_subgraph::NetworkSubgraph,
    config::Cli,
    metrics::handle_serve_metrics,
    query_processor::QueryProcessor,
    server::routes::{network_ratelimiter, slow_ratelimiter},
    util::public_key,
};
// use server::{ServerOptions, index, subgraph_queries, network_queries};

use server::{routes, ServerOptions};

mod allocation_monitor;
mod common;
mod config;
mod graph_node;
mod metrics;
mod model;
mod query_processor;
mod server;
mod util;

/// Create Indexer service App
///
/// Initialization for server and Query processor
///
/// Validate that graph-node instance is running for Query processor
/// Validate that server is running with a health check
///
/// Parse Requests received
///
/// Route the requests as a FreeQuery
///
/// Return response from Query Processor
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    dotenv().ok();

    // Parse basic configurations
    let config = Cli::args();
    let release = package_version().expect("Failed to resolve for release version");

    // Initialize graph-node client
    let graph_node = graph_node::GraphNodeInstance::new(
        &config.indexer_infrastructure.graph_node_query_endpoint,
    );

    // Make an instance of network subgraph at either
    // graph_node_query_endpoint/subgraphs/id/network_subgraph_deployment
    // or network_subgraph_endpoint
    let network_subgraph = NetworkSubgraph::new(
        Some(&config.indexer_infrastructure.graph_node_query_endpoint),
        config
            .network_subgraph
            .network_subgraph_deployment
            .as_deref(),
        &config.network_subgraph.network_subgraph_endpoint,
    );

    let allocation_monitor = allocation_monitor::AllocationMonitor::new(
        network_subgraph.clone(),
        config.ethereum.indexer_address,
        1,
        1000,
        config.ethereum.mnemonic.clone(),
        // TODO: Chain ID should be a config
        U256::from(1),
        // TODO: Dispute manager address should be a config
        Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
    )
    .await
    .expect("Initialize allocation monitor");

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new(graph_node.clone(), allocation_monitor.clone());

    // Start indexer service basic metrics
    tokio::spawn(handle_serve_metrics(
        String::from("0.0.0.0"),
        config.indexer_infrastructure.metrics_port,
    ));
    let service_options = ServerOptions::new(
        Some(config.indexer_infrastructure.port),
        release,
        query_processor,
        config.indexer_infrastructure.free_query_auth_token,
        config.indexer_infrastructure.graph_node_status_endpoint,
        public_key(&config.ethereum.mnemonic).expect("Failed to initiate with operator wallet"),
        network_subgraph,
        config.network_subgraph.network_subgraph_auth_token,
        config.network_subgraph.serve_network_subgraph,
    );

    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

    info!("Initialized server options");
    let app = Router::new()
        .route("/", get(routes::basic::index))
        .route("/health", get(routes::basic::health))
        .route("/version", get(routes::basic::version))
        .route(
            "/status",
            post(routes::status::status_queries)
                .layer(AddExtensionLayer::new(network_ratelimiter())),
        )
        .route(
            "/subgraphs/health/:deployment",
            get(routes::deployment::deployment_health
                .layer(AddExtensionLayer::new(slow_ratelimiter()))),
        )
        // TODO: Add public cost API
        .nest(
            "/operator",
            routes::basic::create_operator_server(service_options.clone())
                .layer(AddExtensionLayer::new(slow_ratelimiter())),
        )
        .route(
            "/network",
            post(routes::network::network_queries)
                .layer(AddExtensionLayer::new(network_ratelimiter())),
        )
        .route(
            "/subgraphs/id/:id",
            post(routes::subgraphs::subgraph_queries),
        )
        .layer(Extension(schema))
        .layer(Extension(service_options.clone()))
        .layer(CorsLayer::new().allow_methods([Method::GET, Method::POST]))
        .layer(
            // Handle error for timeout, ratelimit, or a general internal server error
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        Ok(StatusCode::REQUEST_TIMEOUT)
                    } else {
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Unhandled internal error: {}", error),
                        ))
                    }
                }))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(trace::DefaultMakeSpan::new().level(Level::DEBUG))
                        .on_response(trace::DefaultOnResponse::new().level(Level::DEBUG)),
                )
                .timeout(Duration::from_secs(10))
                .into_inner(),
        );

    let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.indexer_infrastructure.port))
        .expect("Start server port");
    info!("Initialized server app at {}", addr);
    Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    Ok(())
}
