// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, extract::State, response::IntoResponse, Json};
use reqwest::StatusCode;
use serde_json::json;
use tracing::warn;

use indexer_monitor::SubgraphClient;

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
            warn!("Failed to read response body: {}", e);
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

impl IntoResponse for StaticSubgraphError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(%self, "StaticSubgraphError occoured.");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json! {{
                "message": self.to_string(),
            }}),
        )
            .into_response()
    }
}
