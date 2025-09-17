// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, extract::State, response::IntoResponse, Json};
use indexer_monitor::SubgraphClient;
use reqwest::StatusCode;
use serde_json::json;

#[autometrics::autometrics]
pub async fn static_subgraph_request_handler(
    State(subgraph_client): State<&'static SubgraphClient>,
    body: Bytes,
) -> Result<impl IntoResponse, StaticSubgraphError> {
    let response = subgraph_client.query_raw(body).await?;

    Ok((
        response.status(),
        response.headers().to_owned(),
        response.text().await.inspect_err(|e| {
            tracing::warn!(error = %e, "Failed to read response body");
        })?,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum StaticSubgraphError {
    #[error("Failed to query subgraph: {0}")]
    FailedToQuery(#[from] anyhow::Error),

    #[error("Failed to parse subgraph response: {0}")]
    FailedToParse(#[from] reqwest::Error),
}

impl From<&StaticSubgraphError> for StatusCode {
    fn from(value: &StaticSubgraphError) -> Self {
        match value {
            StaticSubgraphError::FailedToQuery(_) => StatusCode::SERVICE_UNAVAILABLE,
            StaticSubgraphError::FailedToParse(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for StaticSubgraphError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(%self, "StaticSubgraphError occurred.");
        (
            StatusCode::from(&self),
            Json(json! {{
                "message": self.to_string(),
            }}),
        )
            .into_response()
    }
}
