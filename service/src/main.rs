// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use anyhow::Error;
use axum::{
    async_trait,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use clap::Parser;
use indexer_common::indexer_service::http::{
    IndexerService, IndexerServiceImpl, IndexerServiceOptions, IndexerServiceRelease, IsAttestable,
};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use thegraph::types::DeploymentId;
use thiserror::Error;
use tracing::error;

mod cli;
mod config;
pub mod database;
mod routes;

use cli::Cli;
use config::Config;

#[derive(Debug, Error)]
pub enum SubgraphServiceError {
    #[error("Invalid status query: {0}")]
    InvalidStatusQuery(Error),
    #[error("Unsupported status query fields: {0:?}")]
    UnsupportedStatusQueryFields(Vec<String>),
    #[error("Internal server error: {0}")]
    StatusQueryError(Error),
}

impl From<&SubgraphServiceError> for StatusCode {
    fn from(err: &SubgraphServiceError) -> Self {
        use SubgraphServiceError::*;
        match err {
            InvalidStatusQuery(_) => StatusCode::BAD_REQUEST,
            UnsupportedStatusQueryFields(_) => StatusCode::BAD_REQUEST,
            StatusQueryError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// Tell axum how to convert `SubgraphServiceError` into a response.
impl IntoResponse for SubgraphServiceError {
    fn into_response(self) -> Response {
        (StatusCode::from(&self), self.to_string()).into_response()
    }
}

#[derive(Serialize)]
#[serde(transparent)]
struct SubgraphResponse {
    inner: Value,
    #[serde(skip)]
    attestable: bool,
}

impl SubgraphResponse {
    fn new(inner: Value, attestable: bool) -> Self {
        Self { inner, attestable }
    }
}

impl IntoResponse for SubgraphResponse {
    fn into_response(self) -> Response {
        Json(self.inner).into_response()
    }
}

impl IsAttestable for SubgraphResponse {
    fn is_attestable(&self) -> bool {
        self.attestable
    }
}

pub struct SubgraphServiceState {
    pub config: Config,
    pub database: PgPool,
    pub cost_schema: routes::cost::CostSchema,
    pub graph_node_client: reqwest::Client,
    pub graph_node_status_url: String,
}

struct SubgraphService {
    config: Config,
}

impl SubgraphService {
    fn new(config: Config) -> Self {
        Self { config }
    }
}

#[async_trait]
impl IndexerServiceImpl for SubgraphService {
    type Error = SubgraphServiceError;
    type Request = serde_json::Value;
    type Response = SubgraphResponse;
    type State = SubgraphServiceState;

    async fn process_request(
        &self,
        _manifest_id: DeploymentId,
        request: Self::Request,
    ) -> Result<(Self::Request, Self::Response), Self::Error> {
        Ok((request, SubgraphResponse::new(json!("hello"), false)))
    }
}

/// Run the subgraph indexer service
#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();

    // Parse command line and environment arguments
    let cli = Cli::parse();

    // Load the json-rpc service configuration, which is a combination of the
    // general configuration options for any indexer service and specific
    // options added for JSON-RPC
    let config = match Config::load(&cli.config) {
        Ok(config) => config,
        Err(e) => {
            error!(
                "Invalid configuration file `{}`: {}",
                cli.config.display(),
                e
            );
            std::process::exit(1);
        }
    };

    // Parse basic configurations
    build_info::build_info!(fn build_info);
    let release = IndexerServiceRelease::from(build_info());

    // Some of the subgrpah service configuration goes into the so-called
    // "state", which will be passed to any request handler, middleware etc.
    // that is involved in serving requests
    let state = Arc::new(SubgraphServiceState {
        config: config.clone(),
        database: database::connect(&config.common.database.postgres_url).await,
        cost_schema: routes::cost::build_schema().await,
        graph_node_client: reqwest::ClientBuilder::new()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to init HTTP client for Graph Node"),
        graph_node_status_url: config
            .common
            .graph_node
            .as_ref()
            .expect("Config must have `common.graph_node.status_url` set")
            .status_url
            .clone(),
    });

    IndexerService::run(IndexerServiceOptions {
        release,
        config: config.common.clone(),
        url_namespace: "subgraphs",
        metrics_prefix: "subgraph",
        service_impl: SubgraphService::new(config),
        extra_routes: Router::new()
            .route("/cost", post(routes::cost::cost))
            .route("/status", post(routes::status))
            .with_state(state),
    })
    .await
}
