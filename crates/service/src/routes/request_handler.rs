// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::{
    error::IndexerServiceError,
    metrics::FAILED_RECEIPT,
    middleware::{Allocation, Sender},
    tap::AgoraQuery,
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    Extension,
};
use axum_extra::TypedHeader;
use reqwest::StatusCode;
use serde_json::value::RawValue;
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::service::{AttestationOutput, IndexerServiceResponse, IndexerServiceState, TapReceipt};

pub async fn request_handler(
    Path(manifest_id): Path<DeploymentId>,
    TypedHeader(receipt): TypedHeader<TapReceipt>,
    Extension(Sender(sender)): Extension<Sender>,
    Extension(Allocation(allocation_id)): Extension<Allocation>,
    State(state): State<Arc<IndexerServiceState>>,
    headers: HeaderMap,
    req: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    trace!("Handling request for deployment `{manifest_id}`");

    let request: QueryBody =
        serde_json::from_str(&req).map_err(|e| IndexerServiceError::InvalidRequest(e.into()))?;

    if let Some(receipt) = receipt.into_signed_receipt() {
        let variables = request
            .variables
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let mut ctx = Context::new();
        ctx.insert(AgoraQuery {
            deployment_id: manifest_id,
            query: request.query.clone(),
            variables,
        });

        // Verify the receipt and store it in the database
        state
            .tap_manager
            .verify_and_store_receipt(&ctx, receipt)
            .await
            .inspect_err(|_| {
                FAILED_RECEIPT
                    .with_label_values(&[
                        &manifest_id.to_string(),
                        &allocation_id.to_string(),
                        &sender.to_string(),
                    ])
                    .inc()
            })
            .map_err(IndexerServiceError::ReceiptError)?;
    } else {
        match headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.to_string())
        {
            None => return Err(IndexerServiceError::Unauthorized),
            Some(ref token) => {
                if Some(token) != state.config.service.free_query_auth_token.as_ref() {
                    return Err(IndexerServiceError::InvalidFreeQueryAuthToken);
                }
            }
        }

        trace!(?manifest_id, "New free query");
    }

    // Check if we have an attestation signer for the allocation the receipt was created for
    let signer = state
        .attestation_signers
        .borrow()
        .get(&allocation_id)
        .cloned()
        .ok_or_else(|| (IndexerServiceError::NoSignerForAllocation(allocation_id)))?;

    let response = state
        .service_impl
        .process_request(manifest_id, request)
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

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct QueryBody {
    pub query: String,
    pub variables: Option<Box<RawValue>>,
}
