// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    body::Bytes,
    extract::State,
    response::{IntoResponse, Response}, Json,
};
use indexer_common::subgraph_client::SubgraphClient;
use reqwest::StatusCode;
use serde::Serialize;
use tracing::warn;

#[autometrics::autometrics]
pub async fn static_subgraph_request_handler(
    State(subgraph_client): State<&'static SubgraphClient>,
    body: Bytes,
) -> Result<impl IntoResponse, StaticSubgraphQueryError> {
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
pub enum StaticSubgraphQueryError {
    #[error("Failed to query subgraph: {0}")]
    FailedToSendRequest(#[from] anyhow::Error),

    #[error("Failed to parse subgraph response: {0}")]
    FailedToParseResponse(#[from] reqwest::Error),
}

impl IntoResponse for StaticSubgraphQueryError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct ErrorResponse {
            error: String,
        }
        tracing::error!(%self, "An IndexerServiceError occoured.");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}
