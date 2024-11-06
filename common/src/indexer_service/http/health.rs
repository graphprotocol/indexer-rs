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
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

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
#[allow(non_snake_case)]
struct IndexingStatus {
    health: Health,
    fatalError: Option<Message>,
    nonFatalErrors: Vec<Message>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "../graphql/indexing_status.schema.graphql",
    query_path = "../graphql/subgraph_health.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct HealthQuery;

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
    #[error("Deployment not found")]
    DeploymentNotFound,
    #[error("Failed to process query")]
    QueryForwardingError,
}

impl IntoResponse for CheckHealthError {
    fn into_response(self) -> AxumResponse {
        let (status, error_message) = match &self {
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
        .await;
    let res = response.expect("Failed to get response");
    let response_json: Result<Response, reqwest::Error> = res.json().await;

    match response_json {
        Ok(res) => {
            if res.data.indexingStatuses.is_empty() {
                return Err(CheckHealthError::DeploymentNotFound);
            };
            let status = &res.data.indexingStatuses[0];
            let health_response = match status.health {
                Health::healthy => json!({ "health": status.health.as_str() }),
                Health::unhealthy => {
                    let errors: Vec<&String> = status
                        .nonFatalErrors
                        .iter()
                        .map(|msg| &msg.message)
                        .collect();
                    json!({ "health": status.health.as_str(), "nonFatalErrors": errors })
                }
                Health::failed => {
                    json!({ "health": status.health.as_str(), "fatalError": status.fatalError.as_ref().map_or("null", |msg| &msg.message) })
                }
            };
            Ok(Json(health_response))
        }
        Err(_) => Err(CheckHealthError::QueryForwardingError),
    }
}
