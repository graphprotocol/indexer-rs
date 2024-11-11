// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{body::Bytes, extract::State, response::IntoResponse};
use indexer_common::subgraph_client::SubgraphClient;
use tracing::warn;

use crate::service::IndexerServiceError;

#[autometrics::autometrics]
pub async fn static_subgraph_request_handler(
    State(subgraph_client): State<&'static SubgraphClient>,
    body: Bytes,
) -> Result<impl IntoResponse, IndexerServiceError> {
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
