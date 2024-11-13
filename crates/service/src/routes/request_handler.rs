// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use indexer_common::{
    attestations::{AttestableResponse, AttestationOutput},
    middleware::tap::QueryBody,
};
use reqwest::StatusCode;
use thegraph_core::DeploymentId;
use tracing::trace;

use alloy::primitives::Address;

use crate::service::{IndexerServiceError, IndexerServiceState};

pub async fn request_handler(
    Path(manifest_id): Path<DeploymentId>,
    State(state): State<Arc<IndexerServiceState>>,
    Extension(allocation_id): Extension<Address>,
    req: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    trace!("Handling request for deployment `{manifest_id}`");

    let request: QueryBody =
        serde_json::from_str(&req).map_err(|e| IndexerServiceError::InvalidRequest(e.into()))?;
    // Check if we have an attestation signer for the allocation the receipt was created for
    let signer = state
        .attestation_signers
        .borrow()
        .get(&allocation_id)
        .cloned()
        .ok_or_else(|| (IndexerServiceError::NoSignerForAllocation(allocation_id)))?;

    let response = state
        .subgraph_service
        .process_request(manifest_id, request)
        .await
        .map_err(IndexerServiceError::ProcessingError)?;

    let res = response.as_str();

    let attestation = AttestationOutput::Attestation(
        response
            .is_attestable()
            .then(|| signer.create_attestation(&req, res)),
    );

    let response = response.finalize(attestation);

    Ok((StatusCode::OK, response))
}
