// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::subgraph_client::Query;
use axum::{
    extract::Path,
    response::{IntoResponse, Response as AxumResponse},
    Extension, Json,
};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

use super::GraphNodeConfig;

#[derive(Deserialize, Debug)]
struct Response {
    data: SubgraphData,
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct SubgraphData {
    indexingStatuses: Vec<IndexingStatus>,
}

#[derive(Deserialize, Debug)]
struct IndexingStatus {
    health: Health,
}

#[derive(Deserialize, Debug)]
#[allow(non_camel_case_types)]
enum Health {
    healthy,
    unhealthy,
    failed,
}

impl Health {
    fn as_str(&self) -> &str {
        match self {
            Health::healthy => "healthy",
            Health::unhealthy => "unhealthy",
            Health::failed => "failed",
        }
    }
}

#[derive(Debug, Error)]
pub enum CheckHealthError {
    #[error("Graph node config not found")]
    GraphNodeConfigNotFound,
    #[error("Deployment not found")]
    DeploymentNotFound,
    #[error("Failed to process query")]
    QueryForwardingError,
}

impl IntoResponse for CheckHealthError {
    fn into_response(self) -> AxumResponse {
        let (status, error_message) = match &self {
            CheckHealthError::GraphNodeConfigNotFound => {
                (StatusCode::NOT_FOUND, "Graph node config not found")
            }
            CheckHealthError::DeploymentNotFound => (StatusCode::NOT_FOUND, "Deployment not found"),
            CheckHealthError::QueryForwardingError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to process query")
            }
        };

        let body = serde_json::json!({
            "error": error_message,
        });

        (status, Json(body)).into_response()
    }
}

pub async fn health(
    Path(deployment_id): Path<String>,
    Extension(graph_node): Extension<Option<GraphNodeConfig>>,
) -> Result<impl IntoResponse, CheckHealthError> {
    let url = if let Some(graph_node) = graph_node {
        graph_node.status_url
    } else {
        return Err(CheckHealthError::GraphNodeConfigNotFound);
    };

    let body = Query::new_with_variables(
        r#"
            query indexingStatuses($ids: [String!]!) {
                indexingStatuses(subgraphs: $ids) {
                    health
                }
            }
        "#,
        [("ids", json!([deployment_id]))],
    );

    let client = reqwest::Client::new();
    let response = client.post(url).json(&body).send().await;
    let response = response.expect("Failed to get response");
    let response_json: Result<Response, reqwest::Error> = response.json().await;

    match response_json {
        Ok(res) => {
            if res.data.indexingStatuses.is_empty() {
                return Err(CheckHealthError::DeploymentNotFound);
            };
            let health_status = res.data.indexingStatuses[0].health.as_str();
            Ok(Json(json!({ "health": health_status })))
        }
        Err(_) => Err(CheckHealthError::QueryForwardingError),
    }
}
