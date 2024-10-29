// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use super::{error::SubgraphServiceError, routes};
use anyhow::anyhow;
use axum::{async_trait, routing::post, Json, Router};
use indexer_common::indexer_service::http::{
    AttestationOutput, IndexerServiceImpl, IndexerServiceResponse,
};
use indexer_config::Config;
use reqwest::Url;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use thegraph_core::DeploymentId;

use crate::{cli::Cli, database};

use clap::Parser;
use indexer_common::indexer_service::http::{
    IndexerService, IndexerServiceOptions, IndexerServiceRelease,
};
use tracing::error;

#[derive(Debug)]
struct SubgraphServiceResponse {
    inner: String,
    attestable: bool,
}

impl SubgraphServiceResponse {
    pub fn new(inner: String, attestable: bool) -> Self {
        Self { inner, attestable }
    }
}

impl IndexerServiceResponse for SubgraphServiceResponse {
    type Data = Json<Value>;
    type Error = SubgraphServiceError; // not used

    fn is_attestable(&self) -> bool {
        self.attestable
    }

    fn as_str(&self) -> Result<&str, Self::Error> {
        Ok(self.inner.as_str())
    }

    fn finalize(self, attestation: AttestationOutput) -> Self::Data {
        let (attestation_key, attestation_value) = match attestation {
            AttestationOutput::Attestation(attestation) => ("attestation", json!(attestation)),
            AttestationOutput::Attestable => ("attestable", json!(self.is_attestable())),
        };
        Json(json!({
            "graphQLResponse": self.inner,
            attestation_key: attestation_value,
        }))
    }
}

pub struct SubgraphServiceState {
    pub config: &'static Config,
    pub database: PgPool,
    pub cost_schema: routes::cost::CostSchema,
    pub graph_node_client: reqwest::Client,
    pub graph_node_status_url: &'static Url,
    pub graph_node_query_base_url: &'static Url,
}

struct SubgraphService {
    state: Arc<SubgraphServiceState>,
}

impl SubgraphService {
    fn new(state: Arc<SubgraphServiceState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl IndexerServiceImpl for SubgraphService {
    type Error = SubgraphServiceError;
    type Response = SubgraphServiceResponse;
    type State = SubgraphServiceState;

    async fn process_request<Request: DeserializeOwned + Send + std::fmt::Debug + Serialize>(
        &self,
        deployment: DeploymentId,
        request: Request,
    ) -> Result<(Request, Self::Response), Self::Error> {
        let deployment_url = self
            .state
            .graph_node_query_base_url
            .join(&format!("subgraphs/id/{deployment}"))
            .map_err(|_| SubgraphServiceError::InvalidDeployment(deployment))?;

        let response = self
            .state
            .graph_node_client
            .post(deployment_url)
            .json(&request)
            .send()
            .await
            .map_err(SubgraphServiceError::QueryForwardingError)?;

        let attestable = response
            .headers()
            .get("graph-attestable")
            .map_or(false, |value| {
                value.to_str().map(|value| value == "true").unwrap_or(false)
            });

        let body = response
            .text()
            .await
            .map_err(SubgraphServiceError::QueryForwardingError)?;

        Ok((request, SubgraphServiceResponse::new(body, attestable)))
    }
}

/// Run the subgraph indexer service
pub async fn run() -> anyhow::Result<()> {
    // Parse command line and environment arguments
    let cli = Cli::parse();

    // Load the json-rpc service configuration, which is a combination of the
    // general configuration options for any indexer service and specific
    // options added for JSON-RPC
    let config = Box::leak(Box::new(
        Config::parse(indexer_config::ConfigPrefix::Service, cli.config.as_ref()).map_err(|e| {
            error!(
                "Invalid configuration file `{}`: {}, if a value is missing you can also use \
                --config to fill the rest of the values",
                cli.config.unwrap_or_default().display(),
                e
            );
            anyhow!(e)
        })?,
    ));

    // Parse basic configurations
    build_info::build_info!(fn build_info);
    let release = IndexerServiceRelease::from(build_info());

    // Some of the subgraph service configuration goes into the so-called
    // "state", which will be passed to any request handler, middleware etc.
    // that is involved in serving requests
    let state = Arc::new(SubgraphServiceState {
        config,
        database: database::connect(config.database.clone().get_formated_postgres_url().as_ref())
            .await,
        cost_schema: routes::cost::build_schema().await,
        graph_node_client: reqwest::ClientBuilder::new()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to init HTTP client for Graph Node"),
        graph_node_status_url: &config.graph_node.status_url,
        graph_node_query_base_url: &config.graph_node.query_url,
    });

    IndexerService::run(IndexerServiceOptions {
        release,
        config,
        url_namespace: "subgraphs",
        service_impl: SubgraphService::new(state.clone()),
        extra_routes: Router::new()
            .route("/cost", post(routes::cost::cost))
            .route("/status", post(routes::status))
            .with_state(state),
    })
    .await
}
