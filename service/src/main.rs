use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use axum::{
    error_handling::HandleErrorLayer,
    extract::{Path, Query},
    http::{Method, StatusCode},
    response::IntoResponse,
    routing::{get, patch},
    Json,
};
use axum::{routing::post, Extension, Router, Server};
use dotenvy::dotenv;
use model::QueryRoot;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::RwLock};
use std::{env, error::Error, sync::Arc, time::Duration};
use tower::{BoxError, ServiceBuilder};
use tower_http::cors::CorsLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use util::package_version;
use uuid::Uuid;

use crate::{query_processor::{FreeQuery, QueryProcessor, SubgraphDeploymentID}, config::Cli, metrics::handle_serve_metrics};
// use server::{ServerOptions, index, subgraph_queries, network_queries};
use common::database::create_pg_pool;
use server::{routes, ServerOptions};

mod common;
mod config;
mod graph_node;
mod model;
mod query_fee;
mod query_processor;
mod receipt_collector;
mod server;
mod util;
mod metrics;

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

    // let database_url = env::var("DATABASE_URL").expect("Postgres URL required.");
    // // Create config for user input args
    // // DATABASE_URL required for using query_as macro
    // let pg_pool = create_pg_pool(&database_url);
    // let arc_pool = Arc::new(pg_pool);

    // let connection = arc_pool.get().expect("Failed to get connection from pool");
    
    
    let release = package_version().expect("Failed to resolve for release version");

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new(
        "http://127.0.0.1:8000",
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli",
    );

    // Start indexer service basic metrics
    tokio::spawn(handle_serve_metrics(String::from("http://0.0.0.0"), config.indexer_infrastructure.metrics_port));

    let service_options = ServerOptions::new(
        Some(8080),
        release,
        query_processor,
        Some("free_query_auth_token".to_string()),
        "graph_node_status_endpoint".to_string(),
        "operator_public_key".to_string(),
        Some("network_subgraph_auth_token".to_string()),
        true,
    );

    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

    info!("Initialized server options");
    let app = Router::new()
        .route("/", get(routes::basic::index))
        .route("/health", get(routes::basic::health))
        .route("/version", get(routes::basic::version))
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
        .layer(Extension(service_options))
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

    info!("Initialized server app at 0.0.0.0::8080");
    Server::bind(&"0.0.0.0:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}
