use alloy::primitives::Address;
use axum::{
    response::{IntoResponse, Response},
    Json,
};
use reqwest::StatusCode;
use serde::Serialize;

use crate::error::SubgraphServiceError;

#[derive(Debug, thiserror::Error)]
pub enum IndexerServiceError {
    // #[error("Issues with provided receipt: {0}")]
    // ReceiptError(tap_core::Error),
    #[error("No attestation signer found for allocation `{0}`")]
    NoSignerForAllocation(Address),
    // #[error("Invalid request body: {0}")]
    // InvalidRequest(anyhow::Error),
    #[error("Error while processing the request: {0}")]
    ProcessingError(SubgraphServiceError),
    // #[error("No valid receipt or free query auth token provided")]
    // Unauthorized,
    // #[error("Invalid free query auth token")]
    // InvalidFreeQueryAuthToken,
    // #[error("Failed to sign attestation")]
    // FailedToSignAttestation,
    #[error("Failed to query subgraph: {0}")]
    FailedToQueryStaticSubgraph(anyhow::Error),
    //
    // #[error("Could not decode signer: {0}")]
    // CouldNotDecodeSigner(tap_core::Error),
    //
    // #[error("There was an error while accessing escrow account: {0}")]
    // EscrowAccount(EscrowAccountsError),
}

impl IntoResponse for IndexerServiceError {
    fn into_response(self) -> Response {
        use IndexerServiceError::*;

        #[derive(Serialize)]
        struct ErrorResponse {
            message: String,
        }

        let status = match self {
            // Unauthorized => StatusCode::UNAUTHORIZED,

            NoSignerForAllocation(_) 
            // | FailedToSignAttestation 
            => StatusCode::INTERNAL_SERVER_ERROR,

            // ReceiptError(_)
            // | InvalidRequest(_)
             // InvalidFreeQueryAuthToken
            // | CouldNotDecodeSigner(_)
            // | EscrowAccount(_)
             ProcessingError(_) => StatusCode::BAD_REQUEST,

            FailedToQueryStaticSubgraph(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        tracing::error!(%self, "An IndexerServiceError occoured.");
        (
            status,
            Json(ErrorResponse {
                message: self.to_string(),
            }),
        )
            .into_response()
    }
}
