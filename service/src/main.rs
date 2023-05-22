use tracing::info;
use util::package_version;
use std::{error::Error, time::Duration, sync::Arc, env};
use axum::{ Router, Server, Extension, routing::post};
use model::QueryRoot;
use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use tower_http::cors::CorsLayer;
use axum::{
    error_handling::HandleErrorLayer,
    extract::{Path, Query},
    http::{StatusCode, Method},
    response::IntoResponse,
    routing::{get, patch},
    Json, 
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{RwLock},
};
use tower::{BoxError, ServiceBuilder};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::query_processor::{FreeQuery, QueryProcessor, SubgraphDeploymentID};
// use server::{ServerOptions, index, subgraph_queries, network_queries};
use server::{ServerOptions, routes};
use common::database::create_pg_pool;

mod query_processor;
mod query_fee;
mod server;
mod receipt_collector;
mod common;
mod util;
mod model;

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
    // env_logger::init();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "service=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = env::var("DATABASE_URL").expect("Postgres URL required.");
    // Create config for user input args
    // DATABASE_URL required for using query_as macro
    let pg_pool = create_pg_pool(&database_url);
    let arc_pool = Arc::new(pg_pool);

    let connection = arc_pool.get().expect("Failed to get connection from pool");
    let release  = package_version().expect("Failed to resolve for release version");

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new("http://127.0.0.1:8000", "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli");

    let service_options = ServerOptions::new(
        Some(8080),
        release,
        query_processor,
        Some("free_query_auth_token".to_string()),
        "graph_node_status_endpoint".to_string(),
        "operator_public_key".to_string(),
        Some("network_subgraph_auth_token".to_string()),
        true
    );

    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

    info!("Initialized server options");
    let app = Router::new()
        .route("/", get(routes::basic::index))
        .route("/health", get(routes::basic::health))
        .route("/version", get(routes::basic::version))
        .route("/subgraphs/id/:id", post(routes::subgraphs::subgraph_queries))
        .route("/network", post(routes::network::network_queries))
        .nest("/operator", routes::basic::create_operator_server(service_options.clone()))
        .layer(Extension(schema))
        .layer(Extension(service_options))
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST]),
        )
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

