// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, http::HeaderMap, response::IntoResponse, Extension, Json};
use reqwest::StatusCode;
use serde_json::json;
use tracing::warn;

use indexer_common::SubgraphClient;

#[autometrics::autometrics]
pub async fn static_subgraph_request_handler(
    Extension(subgraph_client): Extension<&'static SubgraphClient>,
    Extension(required_auth_token): Extension<Option<String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StaticSubgraphError> {
    if let Some(required_auth_token) = required_auth_token {
        let authorization = headers
            .get("authorization")
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| StaticSubgraphError::Unauthorized)?
            .ok_or_else(|| StaticSubgraphError::Unauthorized)?
            .trim_start_matches("Bearer ");

        if authorization != required_auth_token {
            return Err(StaticSubgraphError::Unauthorized);
        }
    }

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
    #[error("No valid receipt or free query auth token provided")]
    Unauthorized,

    #[error("Failed to query subgraph: {0}")]
    FailedToQuery(#[from] anyhow::Error),

    #[error("Failed to parse subgraph response: {0}")]
    FailedToParse(#[from] reqwest::Error),
}

impl IntoResponse for StaticSubgraphError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            StaticSubgraphError::Unauthorized => StatusCode::UNAUTHORIZED,
            StaticSubgraphError::FailedToQuery(_) | StaticSubgraphError::FailedToParse(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        tracing::error!(%self, "StaticSubgraphError occoured.");
        (
            status,
            Json(json! {{
                "message": self.to_string(),
            }}),
        )
            .into_response()
    }
}
