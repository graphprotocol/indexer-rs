// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::convert::Infallible;

use anyhow::Error;
use axum::{
    response::{IntoResponse, Response},
    Json,
};
use indexer_monitor::EscrowAccountsError;
use reqwest::StatusCode;
use serde::Serialize;
use tap_core::{receipt::ReceiptError, Error as TapError};
use thegraph_core::DeploymentId;
use thiserror::Error;

/// Implements `IntoResponse` for error types that implement `StatusCodeExt`.
///
/// This macro reduces boilerplate for service error types by providing a
/// consistent error response pattern:
/// 1. Logs the error at ERROR level with structured tracing
/// 2. Extracts the status code via `StatusCodeExt`
/// 3. Returns a JSON response with `{ "message": "<error>" }` format
///
/// # Example
/// ```ignore
/// impl_service_error_response!(MyError, "MyError");
/// ```
#[macro_export]
macro_rules! impl_service_error_response {
    ($error_type:ty, $error_name:literal) => {
        impl axum::response::IntoResponse for $error_type {
            fn into_response(self) -> axum::response::Response {
                tracing::error!(error = %self, concat!($error_name, " occurred"));
                let status_code = <Self as $crate::error::StatusCodeExt>::status_code(&self);
                $crate::error::ErrorResponse::new(&self).into_response(status_code)
            }
        }
    };
}

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
    TapCoreError(#[from] tap_core::Error),

    #[error("Issues with provided receipt: {0}")]
    Eip712Error(#[from] tap_core::signed_message::Eip712Error),

    #[error("Issues with provided receipt: {0}")]
    InvalidTapReceipt(String),

    #[error("There was an error while accessing escrow account: {0}")]
    EscrowAccount(#[from] EscrowAccountsError),
}

// Helper struct to properly format
// error messages
#[derive(Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct ErrorResponse {
    pub(crate) message: String,
}

impl ErrorResponse {
    pub fn new(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
        }
    }

    pub fn into_response(self, status_code: StatusCode) -> Response {
        (status_code, Json(self)).into_response()
    }
}

impl StatusCodeExt for IndexerServiceError {
    fn status_code(&self) -> StatusCode {
        use IndexerServiceError as E;
        match &self {
            E::TapCoreError(ref error) => match error {
                TapError::ReceiptError(ReceiptError::CheckFailure(_)) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            E::EscrowAccount(_) | E::ReceiptNotFound => StatusCode::PAYMENT_REQUIRED,
            E::DeploymentIdNotFound => StatusCode::INTERNAL_SERVER_ERROR,
            E::AxumError(_) | E::SerializationError(_) => StatusCode::BAD_GATEWAY,
            E::Eip712Error(_) | E::InvalidTapReceipt(_) => StatusCode::BAD_REQUEST,
        }
    }
}

impl_service_error_response!(IndexerServiceError, "IndexerServiceError");

#[derive(Debug, Error)]
pub enum SubgraphServiceError {
    #[error("Invalid status query: {0}")]
    InvalidStatusQuery(#[source] Error),
    #[error("Unsupported status query fields: {0:?}")]
    UnsupportedStatusQueryFields(Vec<String>),
    #[error("Internal server error: {0}")]
    StatusQueryError(#[source] Error),
    #[error("Invalid deployment: {0}")]
    InvalidDeployment(DeploymentId),
    #[error("Failed to process query: {0}")]
    QueryForwardingError(#[source] reqwest::Error),
}

impl StatusCodeExt for SubgraphServiceError {
    fn status_code(&self) -> StatusCode {
        use SubgraphServiceError::*;
        match self {
            InvalidStatusQuery(_) | UnsupportedStatusQueryFields(_) => StatusCode::BAD_REQUEST,
            InvalidDeployment(_) => StatusCode::INTERNAL_SERVER_ERROR,
            StatusQueryError(_) => StatusCode::BAD_GATEWAY,
            QueryForwardingError(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl_service_error_response!(SubgraphServiceError, "SubgraphServiceError");

pub trait StatusCodeExt {
    fn status_code(&self) -> StatusCode;
}

impl<T> StatusCodeExt for Response<T> {
    fn status_code(&self) -> StatusCode {
        self.status()
    }
}

impl<T, E> StatusCodeExt for Result<T, E>
where
    T: StatusCodeExt,
    E: StatusCodeExt,
{
    fn status_code(&self) -> StatusCode {
        match self {
            Ok(t) => t.status_code(),
            Err(e) => e.status_code(),
        }
    }
}

impl StatusCodeExt for Infallible {
    fn status_code(&self) -> StatusCode {
        unreachable!()
    }
}
