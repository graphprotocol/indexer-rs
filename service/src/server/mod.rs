// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    error_handling::HandleErrorLayer,
    handler::Handler,
    http::{Method, StatusCode},
    routing::get,
};
use axum::{routing::post, Extension, Router};
use sqlx::PgPool;

use std::time::Duration;
use tower::{BoxError, ServiceBuilder};
use tower_http::{
    add_extension::AddExtensionLayer,
    cors::CorsLayer,
    trace::{self, TraceLayer},
};
use tracing::Level;

use crate::{
    common::network_subgraph::NetworkSubgraph,
    query_processor::QueryProcessor,
    server::routes::{network_ratelimiter, slow_ratelimiter},
    util::PackageVersion,
};

pub mod routes;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub port: Option<u32>,
    pub release: PackageVersion,
    pub query_processor: QueryProcessor,
    pub free_query_auth_token: Option<String>,
    pub graph_node_status_endpoint: String,
    pub indexer_management_db: PgPool,
    pub operator_public_key: String,
    pub network_subgraph: NetworkSubgraph,
    pub network_subgraph_auth_token: Option<String>,
    pub serve_network_subgraph: bool,
}

impl ServerOptions {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        port: Option<u32>,
        release: PackageVersion,
        query_processor: QueryProcessor,
        free_query_auth_token: Option<String>,
        graph_node_status_endpoint: String,
        indexer_management_db: PgPool,
        operator_public_key: String,
        network_subgraph: NetworkSubgraph,
        network_subgraph_auth_token: Option<String>,
        serve_network_subgraph: bool,
    ) -> Self {
        let free_query_auth_token = free_query_auth_token.map(|token| format!("Bearer {}", token));

        ServerOptions {
            port,
            release,
            query_processor,
            free_query_auth_token,
            graph_node_status_endpoint,
            indexer_management_db,
            operator_public_key,
            network_subgraph,
            network_subgraph_auth_token,
            serve_network_subgraph,
        }
    }
}

pub async fn create_server(options: ServerOptions) -> Router {
    Router::new()
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
        .route(
            "/cost",
            post(routes::cost::graphql_handler)
                .get(routes::cost::graphql_handler)
                .layer(AddExtensionLayer::new(slow_ratelimiter())),
        )
        .nest(
            "/operator",
            routes::basic::create_operator_server(options.clone())
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
        .layer(Extension(options.clone()))
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
        )
}
