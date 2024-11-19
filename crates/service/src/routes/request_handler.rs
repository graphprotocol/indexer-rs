// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::tap::AgoraQuery;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use axum_extra::TypedHeader;
use lazy_static::lazy_static;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use reqwest::StatusCode;
use serde_json::value::RawValue;
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::service::{
    AttestationOutput, IndexerServiceError, IndexerServiceResponse, IndexerServiceState, TapReceipt,
};

lazy_static! {
    /// Register indexer error metrics in Prometheus registry
    pub static ref HANDLER_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "indexer_query_handler_seconds",
        "Histogram for default indexer query handler",
        &["deployment", "allocation", "sender"]
    ).unwrap();

    pub static ref HANDLER_FAILURE: CounterVec = register_counter_vec!(
        "indexer_query_handler_failed_total",
        "Failed queries to handler",
        &["deployment"]
    ).unwrap();

    pub static ref FAILED_RECEIPT: CounterVec = register_counter_vec!(
        "indexer_receipt_failed_total",
        "Failed receipt checks",
        &["deployment", "allocation", "sender"]
    ).unwrap();

}

pub async fn request_handler(
    Path(manifest_id): Path<DeploymentId>,
    typed_header: TypedHeader<TapReceipt>,
    state: State<Arc<IndexerServiceState>>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    _request_handler(manifest_id, typed_header, state, headers, body)
        .await
        .inspect_err(|_| {
            HANDLER_FAILURE
                .with_label_values(&[&manifest_id.to_string()])
                .inc()
        })
}

async fn _request_handler(
    manifest_id: DeploymentId,
    TypedHeader(receipt): TypedHeader<TapReceipt>,
    State(state): State<Arc<IndexerServiceState>>,
    headers: HeaderMap,
    req: String,
) -> Result<impl IntoResponse, IndexerServiceError> {
    trace!("Handling request for deployment `{manifest_id}`");

    let request: QueryBody =
        serde_json::from_str(&req).map_err(|e| IndexerServiceError::InvalidRequest(e.into()))?;

    let Some(receipt) = receipt.into_signed_receipt() else {
        // Serve free query, NO METRICS
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

        let response = state
            .service_impl
            .process_request(manifest_id, request)
            .await
            .map_err(IndexerServiceError::ProcessingError)?
            .finalize(AttestationOutput::Attestable);
        return Ok((StatusCode::OK, response));
    };

    let allocation_id = receipt.message.allocation_id;

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

    // recover the signer address
    // get escrow accounts from channel
    // return sender from signer
    //
    // TODO: We are currently doing this process twice.
    // One here and other on common/src/tap/checks/sender_balance_check.rs
    // We'll get back to normal once we have attachable context to `verify_and_store_receipt`
    let signer = receipt
        .recover_signer(&state.domain_separator)
        .map_err(IndexerServiceError::CouldNotDecodeSigner)?;
    let sender = state
        .escrow_accounts
        .borrow()
        .get_sender_for_signer(&signer)
        .map_err(IndexerServiceError::EscrowAccount)?;

    let _metric = HANDLER_HISTOGRAM
        .with_label_values(&[
            &manifest_id.to_string(),
            &allocation_id.to_string(),
            &sender.to_string(),
        ])
        .start_timer();

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
