// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response as AxumResponse},
    Json,
};
use graphql_client::GraphQLQuery;
use indexer_config::GraphNodeConfig;
use indexer_query::{health_query, HealthQuery};
use reqwest::StatusCode;
use serde_json::json;
use thiserror::Error;

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
        let status = match &self {
            CheckHealthError::DeploymentNotFound => StatusCode::NOT_FOUND,
            CheckHealthError::InvalidHealthStatus | CheckHealthError::BadResponse => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            CheckHealthError::RequestFailed => StatusCode::BAD_GATEWAY,
        };
        let body = serde_json::json!({
            "error": self.to_string(),
        });
        (status, Json(body)).into_response()
    }
}

pub async fn health(
    Path(deployment_id): Path<String>,
    State(graph_node): State<GraphNodeConfig>,
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
        _ => return Err(CheckHealthError::BadResponse),
    };

    let Some(status) = data.indexing_statuses.first() else {
        return Err(CheckHealthError::DeploymentNotFound);
    };
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
