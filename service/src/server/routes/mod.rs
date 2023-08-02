use crate::common::indexer_error::{IndexerError, IndexerErrorCause};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use hyper::http::HeaderName;

pub mod basic;
pub mod network;
pub mod status;
pub mod subgraphs;

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

pub fn bad_request_response(error_body: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::response::AppendHeaders([(HeaderName::from_static("graph-attestable"), "false")]),
        Json(error_body.to_string()),
    )
        .into_response()
}
