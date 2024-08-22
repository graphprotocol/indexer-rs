// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use axum_extra::TypedHeader;
use reqwest::StatusCode;
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::indexer_service::http::IndexerServiceResponse;

use super::{
    indexer_service::{IndexerServiceError, IndexerServiceState},
    tap_receipt_header::TapReceipt,
    IndexerServiceImpl,
};

#[autometrics::autometrics]
pub async fn request_handler<I>(
    Path(manifest_id): Path<DeploymentId>,
    TypedHeader(receipt): TypedHeader<TapReceipt>,
    State(state): State<Arc<IndexerServiceState<I>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, IndexerServiceError<I::Error>>
where
    I: IndexerServiceImpl + Sync + Send + 'static,
{
    trace!("Handling request for deployment `{manifest_id}`");

    state
        .metrics
        .requests
        .with_label_values(&[&manifest_id.to_string()])
        .inc();

    let request =
        serde_json::from_slice(&body).map_err(|e| IndexerServiceError::InvalidRequest(e.into()))?;

    let attestation_signer = if let Some(receipt) = receipt.into_signed_receipt() {
        let allocation_id = receipt.message.allocation_id;

        // Verify the receipt and store it in the database
        // TODO update checks
        state
            .tap_manager
            .verify_and_store_receipt(receipt)
            .await
            .map_err(IndexerServiceError::ReceiptError)?;

        // Check if we have an attestation signer for the allocation the receipt was created for
        let signers = state
            .attestation_signers
            .value_immediate()
            .ok_or_else(|| IndexerServiceError::ServiceNotReady)?;

        Some(
            signers
                .get(&allocation_id)
                .cloned()
                .ok_or_else(|| (IndexerServiceError::NoSignerForAllocation(allocation_id)))?,
        )
    } else {
        match headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.to_string())
        {
            None => return Err(IndexerServiceError::Unauthorized),
            Some(ref token) => {
                if Some(token) != state.config.server.free_query_auth_token.as_ref() {
                    return Err(IndexerServiceError::InvalidFreeQueryAuthToken);
                }
            }
        }
        None
    };

    let (request, response) = state
        .service_impl
        .process_request(manifest_id, request)
        .await
        .map_err(IndexerServiceError::ProcessingError)?;

    let attestation = match (response.is_attestable(), attestation_signer) {
        (true, Some(signer)) => {
            let req = serde_json::to_string(&request)
                .map_err(|_| IndexerServiceError::FailedToSignAttestation)?;
            let res = response
                .as_str()
                .map_err(|_| IndexerServiceError::FailedToSignAttestation)?;
            Some(signer.create_attestation(&req, res))
        }
        _ => None,
    };

    let response = response.finalize(attestation);

    Ok((StatusCode::OK, response))
}
