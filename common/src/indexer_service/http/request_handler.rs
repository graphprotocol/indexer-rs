use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue},
    response::IntoResponse,
    TypedHeader,
};
use log::info;
use reqwest::StatusCode;
use thegraph::types::DeploymentId;
use tracing::info;

use crate::{indexer_service::http::IsAttestable, prelude::AttestationSigner};

use super::{
    indexer_service::{IndexerServiceError, IndexerServiceState},
    scalar_receipt_header::ScalarReceipt,
    IndexerServiceImpl,
};

pub async fn request_handler<I>(
    Path(manifest_id): Path<DeploymentId>,
    TypedHeader(receipt): TypedHeader<ScalarReceipt>,
    State(state): State<Arc<IndexerServiceState<I>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, IndexerServiceError<I::Error>>
where
    I: IndexerServiceImpl + Sync + Send + 'static,
{
    info!("Handling request for deployment `{manifest_id}`");

    let request =
        serde_json::from_slice(&body).map_err(|e| IndexerServiceError::InvalidRequest(e.into()))?;

    let mut attestation_signer: Option<AttestationSigner> = None;

    if let Some(receipt) = receipt.into_signed_receipt() {
        let allocation_id = receipt.message.allocation_id;

        // Verify the receipt and store it in the database
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

        attestation_signer = Some(
            signers
                .get(&allocation_id)
                .cloned()
                .ok_or_else(|| (IndexerServiceError::NoSignerForAllocation(allocation_id)))?,
        );
    } else if state.config.server.free_query_auth_token.is_some()
        && state.config.server.free_query_auth_token
            != headers
                .get("free-query-auth-token")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
    {
        return Err(IndexerServiceError::Unauthorized);
    }

    let (request, response) = state
        .service_impl
        .process_request(manifest_id, request)
        .await
        .map_err(IndexerServiceError::ProcessingError)?;

    let attestation = match (response.is_attestable(), attestation_signer) {
        (false, _) => None,
        (true, None) => return Err(IndexerServiceError::NoSignerForManifest(manifest_id)),
        (true, Some(signer)) => {
            let req = serde_json::to_string(&request)
                .map_err(|_| IndexerServiceError::FailedToSignAttestation)?;
            let res = serde_json::to_string(&response)
                .map_err(|_| IndexerServiceError::FailedToSignAttestation)?;
            Some(signer.create_attestation(&req, &res))
        }
    };

    let mut headers = HeaderMap::new();
    if let Some(attestation) = attestation {
        let raw_attestation = serde_json::to_string(&attestation)
            .map_err(|_| IndexerServiceError::FailedToProvideAttestation)?;
        let header_value = HeaderValue::from_str(&raw_attestation)
            .map_err(|_| IndexerServiceError::FailedToProvideAttestation)?;
        headers.insert("graph-attestation", header_value);
    }

    Ok((StatusCode::OK, headers, response))
}
