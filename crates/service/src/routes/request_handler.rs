// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::{error::IndexerServiceError, middleware::Allocation};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use reqwest::StatusCode;
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::service::{AttestationOutput, IndexerServiceResponse, IndexerServiceState};

pub async fn request_handler(
    Path(manifest_id): Path<DeploymentId>,
    Extension(Allocation(allocation_id)): Extension<Allocation>,
    State(state): State<Arc<IndexerServiceState>>,
    req: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    trace!("Handling request for deployment `{manifest_id}`");

    // Check if we have an attestation signer for the allocation the receipt was created for
    let signer = state
        .attestation_signers
        .borrow()
        .get(&allocation_id)
        .cloned()
        .ok_or_else(|| (IndexerServiceError::NoSignerForAllocation(allocation_id)))?;

    let response = state
        .service_impl
        .process_request(manifest_id, &req)
        .await
        .map_err(IndexerServiceError::ProcessingError)?;

    let res = response
        .as_str()
        .map_err(|_| IndexerServiceError::FailedToSignAttestation)?;

    let attestation = AttestationOutput::Attestation(
        response
            .is_attestable()
            .then(|| signer.create_attestation(&req, res)),
    );

    let response = response.finalize(attestation);

    Ok((StatusCode::OK, response))
}
