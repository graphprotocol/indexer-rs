// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::Address;
use anyhow::Error;
use axum::{
    response::{IntoResponse, Response},
    Json,
};
use indexer_monitor::EscrowAccountsError;
use reqwest::StatusCode;
use serde::Serialize;
use thegraph_core::DeploymentId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexerServiceError {
    #[error("No Tap receipt was found in the request")]
    ReceiptNotFound,
    #[error("Could not find deployment id")]
    DeploymentIdNotFound,
    #[error(transparent)]
    AxumError(#[from] axum::Error),

    #[error(transparent)]
    SerializationError(#[from] serde_json::Error),

    #[error("Issues with provided receipt: {0}")]
    ReceiptError(#[from] tap_core::Error),
    #[error("No attestation signer found for allocation `{0}`")]
    NoSignerForAllocation(Address),
    #[error("Error while processing the request: {0}")]
    ProcessingError(SubgraphServiceError),
    #[error("Failed to sign attestation")]
    FailedToSignAttestation,

    #[error("There was an error while accessing escrow account: {0}")]
    EscrowAccount(#[from] EscrowAccountsError),
}

impl IntoResponse for IndexerServiceError {
    fn into_response(self) -> Response {
        use IndexerServiceError::*;

        #[derive(Serialize)]
        struct ErrorResponse {
            message: String,
        }

        let status = match self {
            NoSignerForAllocation(_) | FailedToSignAttestation => StatusCode::INTERNAL_SERVER_ERROR,

            ReceiptError(_) | EscrowAccount(_) | ProcessingError(_) => StatusCode::BAD_REQUEST,
            ReceiptNotFound => StatusCode::PAYMENT_REQUIRED,
            DeploymentIdNotFound => StatusCode::INTERNAL_SERVER_ERROR,
            AxumError(_) => StatusCode::BAD_REQUEST,
            SerializationError(_) => StatusCode::BAD_REQUEST,
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

#[derive(Debug, Error)]
pub enum SubgraphServiceError {
    #[error("Invalid status query: {0}")]
    InvalidStatusQuery(Error),
    #[error("Unsupported status query fields: {0:?}")]
    UnsupportedStatusQueryFields(Vec<String>),
    #[error("Internal server error: {0}")]
    StatusQueryError(Error),
    #[error("Invalid deployment: {0}")]
    InvalidDeployment(DeploymentId),
    #[error("Failed to process query: {0}")]
    QueryForwardingError(reqwest::Error),
}

impl From<&SubgraphServiceError> for StatusCode {
    fn from(err: &SubgraphServiceError) -> Self {
        use SubgraphServiceError::*;
        match err {
            InvalidStatusQuery(_) => StatusCode::BAD_REQUEST,
            UnsupportedStatusQueryFields(_) => StatusCode::BAD_REQUEST,
            StatusQueryError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            InvalidDeployment(_) => StatusCode::BAD_REQUEST,
            QueryForwardingError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// Tell axum how to convert `SubgraphServiceError` into a response.
impl IntoResponse for SubgraphServiceError {
    fn into_response(self) -> Response {
        (StatusCode::from(&self), self.to_string()).into_response()
    }
}
