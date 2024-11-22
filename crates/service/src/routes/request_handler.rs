// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::error::IndexerServiceError;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
};
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::service::IndexerServiceState;

pub async fn request_handler(
    Path(manifest_id): Path<DeploymentId>,
    State(state): State<Arc<IndexerServiceState>>,
    req: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    trace!("Handling request for deployment `{manifest_id}`");

    let response = state
        .service_impl
        .process_request(manifest_id, req)
        .await
        .map_err(IndexerServiceError::ProcessingError)?;

    Ok(response)
}
