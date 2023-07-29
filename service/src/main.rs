use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use axum::{
    error_handling::HandleErrorLayer,
    http::{Method, StatusCode},
    routing::get,
};
use axum::{routing::post, Extension, Router, Server};
use dotenvy::dotenv;
use model::QueryRoot;
use reqwest::Client;
use serde_json::json;

use std::{net::SocketAddr, str::FromStr, time::Duration};
use tower::{BoxError, ServiceBuilder};
use tower_http::cors::CorsLayer;
use tracing::info;

use util::package_version;

use crate::{
    config::Cli, metrics::handle_serve_metrics, query_processor::QueryProcessor, util::public_key,
};
// use server::{ServerOptions, index, subgraph_queries, network_queries};

use server::{routes, ServerOptions};

mod common;
mod config;
mod graph_node;
mod metrics;
mod model;
mod query_fee;
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

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new(
        &config.indexer_infrastructure.graph_node_query_endpoint,
        &config.network_subgraph.network_subgraph_endpoint,
    );

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
        config.network_subgraph.network_subgraph_auth_token,
        config.network_subgraph.serve_network_subgraph,
    );

    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

    info!("Initialized server options");
    let app = Router::new()
        .route("/", get(routes::basic::index))
        .route("/health", get(routes::basic::health))
        .route("/version", get(routes::basic::version))
        // .route("/status", post(routes::status::status_middleware))
        .route(
            "/subgraphs/id/:id",
            post(routes::subgraphs::subgraph_queries),
        )
        .route("/network", post(routes::network::network_queries))
        .nest(
            "/operator",
            routes::basic::create_operator_server(service_options.clone()),
        )
        .layer(Extension(schema))
        .layer(Extension(service_options.clone()))
        .layer(CorsLayer::new().allow_methods([Method::GET, Method::POST]))
        .layer(
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
                .timeout(Duration::from_secs(10))
                .into_inner(),
        );

    let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.indexer_infrastructure.port))
        .expect("Start server port");
    info!("Initialized server app at {}", addr);
    Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install CTRL+C signal handler");
        })
        .await
        .unwrap();

    let test_res = test_graphql_query().await;
    println!("{:#?}", test_res);

    Ok(())
}

async fn test_graphql_query() -> serde_json::Value {
    let addr = "http://localhost:7300/subgraphs/id/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj";
    let client = Client::new();

    // Create the JSON data for the GraphQL query
    let query_data = json!({
        "query": "{_meta{block{number}}}"
    });

    // Make the POST request to the server
    let response = client
        .post(addr)
        .json(&query_data)
        .send()
        .await
        .expect("Failed to send request");

    // Assert that the response status is 200 OK
    assert!(response.status().is_success());

    // Read the response body as a JSON value
    let response_json: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse response JSON");

    // Add any additional assertions based on the expected response
    // For example, you can check the values in the response JSON.

    // For debugging purposes, you can print the response JSON
    println!("Response JSON: {:?}", response_json);
    response_json
}
