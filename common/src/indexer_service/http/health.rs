// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::Path,
    response::{IntoResponse, Response as AxumResponse},
    Extension, Json,
};
use graphql_client::GraphQLQuery;
use indexer_config::GraphNodeConfig;
use reqwest::StatusCode;
use serde_json::json;
use thiserror::Error;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "../graphql/indexing_status.schema.graphql",
    query_path = "../graphql/subgraph_health.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct HealthQuery;

#[derive(Debug, Error)]
pub enum CheckHealthError {
    #[error("Failed to send request")]
    RequestFailed,
    #[error("Failed to decode response")]
    BadResponse,
    #[error("Deployment not found")]
    DeploymentNotFound,
    #[error("Invalid health status found")]
    InvalidHealthStatus,
}

impl IntoResponse for CheckHealthError {
    fn into_response(self) -> AxumResponse {
        let (status, error_message) = match &self {
            CheckHealthError::DeploymentNotFound => (StatusCode::NOT_FOUND, "Deployment not found"),
            CheckHealthError::BadResponse => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to decode response",
            ),
            CheckHealthError::RequestFailed => (StatusCode::BAD_GATEWAY, "Failed to send request"),
            CheckHealthError::InvalidHealthStatus => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Invalid health status found",
            ),
        };

        let body = serde_json::json!({
            "error": error_message,
        });

        (status, Json(body)).into_response()
    }
}

pub async fn health(
    Path(deployment_id): Path<String>,
    Extension(graph_node): Extension<GraphNodeConfig>,
) -> Result<impl IntoResponse, CheckHealthError> {
    let req_body = HealthQuery::build_query(health_query::Variables {
        ids: vec![deployment_id],
    });

    let client = reqwest::Client::new();
    let response = client
        .post(graph_node.status_url)
        .json(&req_body)
        .send()
        .await
        .map_err(|_| CheckHealthError::RequestFailed)?;

    let graphql_response: graphql_client::Response<health_query::ResponseData> = response
        .json()
        .await
        .map_err(|_| CheckHealthError::BadResponse)?;

    let data = match (graphql_response.data, graphql_response.errors) {
        (Some(data), None) => data,
        (_, Some(_)) => return Err(CheckHealthError::BadResponse),
        (_, _) => return Err(CheckHealthError::BadResponse),
    };

    if data.indexing_statuses.is_empty() {
        return Err(CheckHealthError::DeploymentNotFound);
    }

    let status = &data.indexing_statuses[0];
    let health_response = match status.health {
        health_query::Health::healthy => json!({ "health": status.health }),
        health_query::Health::unhealthy => {
            let errors: Vec<&String> = status
                .non_fatal_errors
                .iter()
                .map(|msg| &msg.message)
                .collect();
            json!({ "health": status.health, "nonFatalErrors": errors })
        }
        health_query::Health::failed => {
            json!({ "health": status.health, "fatalError": status.fatal_error.as_ref().map_or("null", |msg| &msg.message) })
        }
        health_query::Health::Other(_) => return Err(CheckHealthError::InvalidHealthStatus),
    };
    Ok(Json(health_response))
}
