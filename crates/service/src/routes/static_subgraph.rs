// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, extract::State, response::IntoResponse};
use indexer_monitor::{SubgraphClient, SubgraphQueryError};
use reqwest::StatusCode;

use crate::error::StatusCodeExt;

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
    #[error("Failed to query subgraph")]
    SubgraphQuery(#[from] SubgraphQueryError),

    #[error("Failed to parse subgraph response")]
    ResponseParse(#[from] reqwest::Error),
}

impl StatusCodeExt for StaticSubgraphError {
    fn status_code(&self) -> StatusCode {
        match self {
            StaticSubgraphError::SubgraphQuery(_) => StatusCode::SERVICE_UNAVAILABLE,
            StaticSubgraphError::ResponseParse(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

crate::impl_service_error_response!(StaticSubgraphError, "StaticSubgraphError");
