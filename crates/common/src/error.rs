use axum::{response::IntoResponse, Json};
use reqwest::StatusCode;
use serde::Serialize;

use crate::escrow_accounts::EscrowAccountsError;

#[derive(thiserror::Error, Debug)]
pub enum IndexerError {
    #[error("Could not find deployment id")]
    DeploymentIdNotFound,

    #[error(transparent)]
    AxumError(#[from] axum::Error),

    #[error(transparent)]
    SerializationError(#[from] serde_json::Error),

    #[error(transparent)]
    EscrowAccountsError(#[from] EscrowAccountsError),

    #[error(transparent)]
    TapCoreError(#[from] tap_core::Error),
}

impl IntoResponse for IndexerError {
    fn into_response(self) -> axum::response::Response {
        let status_code = match self {
            IndexerError::DeploymentIdNotFound => StatusCode::INTERNAL_SERVER_ERROR,
            IndexerError::AxumError(_)
            | IndexerError::SerializationError(_)
            | IndexerError::EscrowAccountsError(_) => StatusCode::BAD_REQUEST,
            IndexerError::TapCoreError(_) => StatusCode::BAD_REQUEST,
        };

        #[derive(Serialize)]
        struct ErrorResponse {
            message: String,
        }

        (
            status_code,
            Json(ErrorResponse {
                message: self.to_string(),
            }),
        )
            .into_response()
    }
}
