// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::Extension,
    http::{self, Request},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

use crate::server::ServerOptions;

use super::bad_request_response;

pub async fn network_queries(
    Extension(server): Extension<ServerOptions>,
    req: Request<axum::body::Body>,
    axum::extract::Json(body): axum::extract::Json<Value>,
) -> impl IntoResponse {
    // Extract free query auth token
    let auth_token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|t| t.to_str().ok());

    // Serve only if enabled by indexer and request auth token matches
    if !(server.serve_network_subgraph
        && auth_token.is_some()
        && server.network_subgraph_auth_token.is_some()
        && auth_token.unwrap() == server.network_subgraph_auth_token.as_deref().unwrap())
    {
        return bad_request_response("Not enabled or authorized query");
    }

    match server.network_subgraph.query::<Value>(&body).await {
        Ok(result) => Json(json!({
            "data": result.data,
            "errors": result.errors.map(|errors| {
                errors
                    .into_iter()
                    .map(|e| json!({ "message": e.message }))
                    .collect::<Vec<_>>()
            }),
        }))
        .into_response(),
        Err(e) => bad_request_response(&e.to_string()),
    }
}
