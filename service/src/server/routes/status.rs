// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use axum::{
    http::{Request, StatusCode},
    response::IntoResponse,
    Extension, Json,
};

use hyper::body::Bytes;

use reqwest::{header, Client};

use crate::server::ServerOptions;
use indexer_common::graphql::filter_supported_fields;

use super::bad_request_response;

lazy_static::lazy_static! {
    static ref SUPPORTED_ROOT_FIELDS: HashSet<&'static str> =
        vec![
            "indexingStatuses",
            "chains",
            "latestBlock",
            "earliestBlock",
            "publicProofsOfIndexing",
            "entityChangesInBlock",
            "blockData",
            "cachedEthereumCalls",
            "subgraphFeatures",
            "apiVersions",
        ].into_iter().collect();
}

// Custom middleware function to process the request before reaching the main handler
pub async fn status_queries(
    Extension(server): Extension<ServerOptions>,
    req: Request<axum::body::Body>,
) -> impl IntoResponse {
    let body_bytes = hyper::body::to_bytes(req.into_body()).await.unwrap();
    // Read the requested query string
    let query_string = match String::from_utf8(body_bytes.to_vec()) {
        Ok(s) => s,
        Err(e) => return bad_request_response(&e.to_string()),
    };

    // filter supported root fields
    let query_string = match filter_supported_fields(&query_string, &SUPPORTED_ROOT_FIELDS) {
        Ok(query) => query,
        Err(unsupported_fields) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Cannot query field: {:#?}", unsupported_fields),
            )
                .into_response();
        }
    };

    // Pass the modified operation to the actual endpoint
    let request = Client::new()
        .post(&server.graph_node_status_endpoint)
        .body(Bytes::from(query_string))
        .header(header::CONTENT_TYPE, "application/json");

    match request.send().await {
        Ok(r) => match r.json::<Box<serde_json::value::RawValue>>().await {
            Ok(r) => (StatusCode::OK, Json(r)).into_response(),
            Err(e) => return bad_request_response(&e.to_string()),
        },
        Err(e) => return bad_request_response(&e.to_string()),
    }
}
