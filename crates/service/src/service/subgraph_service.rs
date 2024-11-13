use std::sync::Arc;

use reqwest::Url;
use serde::Serialize;
use sqlx::Postgres;
use thegraph_core::{DeploymentId};

use crate::{error::SubgraphServiceError, routes};

use super::SubgraphServiceResponse;

pub struct SubgraphServiceState {
    pub database: sqlx::Pool<Postgres>,

    pub cost_schema: routes::cost::CostSchema,

    pub graph_node_client: reqwest::Client,
    pub graph_node_query_base_url: &'static Url,
    pub graph_node_status_url: &'static Url,
}

pub struct SubgraphService {
    state: Arc<SubgraphServiceState>,
}

impl SubgraphService {
    pub fn new(state: Arc<SubgraphServiceState>) -> Self {
        Self { state }
    }
}

impl SubgraphService {
    pub async fn process_request<Request: Serialize>(
        &self,
        deployment: DeploymentId,
        request: Request,
    ) -> Result<SubgraphServiceResponse, SubgraphServiceError> {
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

        Ok(SubgraphServiceResponse::new(body, attestable))
    }
}
