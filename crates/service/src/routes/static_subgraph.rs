// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, http::HeaderMap, response::IntoResponse, Extension};
use indexer_common::subgraph_client::SubgraphClient;
use tracing::warn;

use crate::service::IndexerServiceError;

#[autometrics::autometrics]
pub async fn static_subgraph_request_handler(
    Extension(subgraph_client): Extension<&'static SubgraphClient>,
    Extension(required_auth_token): Extension<Option<String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, IndexerServiceError> {
    if let Some(required_auth_token) = required_auth_token {
        let authorization = headers
            .get("authorization")
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| IndexerServiceError::Unauthorized)?
            .ok_or_else(|| IndexerServiceError::Unauthorized)?
            .trim_start_matches("Bearer ");

        if authorization != required_auth_token {
            return Err(IndexerServiceError::Unauthorized);
        }
    }

    let response = subgraph_client
        .query_raw(body)
        .await
        .map_err(IndexerServiceError::FailedToQueryStaticSubgraph)?;

    Ok((
        response.status(),
        response.headers().to_owned(),
        response
            .text()
            .await
            .map_err(|e| {
                warn!("Failed to read response body: {}", e);
                e
            })
            .map_err(|e| IndexerServiceError::FailedToQueryStaticSubgraph(e.into()))?,
    ))
}
