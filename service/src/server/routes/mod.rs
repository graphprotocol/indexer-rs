// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::common::indexer_error::{IndexerError, IndexerErrorCause};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use hyper::http::HeaderName;
use tower::limit::RateLimitLayer;

pub mod basic;
pub mod cost;
pub mod deployment;
pub mod network;
pub mod status;
pub mod subgraphs;

/// Helper function to convert response body to query string
pub async fn response_body_to_query_string(
    body: hyper::body::Body,
) -> Result<String, IndexerError> {
    let query_bytes = hyper::body::to_bytes(body).await.map_err(|e| {
        IndexerError::new(
            crate::common::indexer_error::IndexerErrorCode::IE075,
            Some(IndexerErrorCause::new(e)),
        )
    })?;
    let query_string = String::from_utf8(query_bytes.to_vec()).map_err(|e| {
        IndexerError::new(
            crate::common::indexer_error::IndexerErrorCode::IE075,
            Some(IndexerErrorCause::new(e)),
        )
    })?;
    Ok(query_string)
}

/// Create response for a bad request
pub fn bad_request_response(error_body: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::response::AppendHeaders([(HeaderName::from_static("graph-attestable"), "false")]),
        Json(error_body.to_string()),
    )
        .into_response()
}

/// Create response for an internal server error
pub fn internal_server_error_response(error_body: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(error_body.to_string()),
    )
        .into_response()
}

/// Limit status requests to 9000/30min (5/s)
pub fn slow_ratelimiter() -> RateLimitLayer {
    RateLimitLayer::new(9000, std::time::Duration::from_millis(30 * 60 * 1000))
}

/// Limit network requests to 90000/30min (50/s)
pub fn network_ratelimiter() -> RateLimitLayer {
    RateLimitLayer::new(90000, std::time::Duration::from_millis(30 * 60 * 1000))
}
