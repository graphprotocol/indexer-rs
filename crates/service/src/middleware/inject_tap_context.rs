//! Injects tap context to be used by the checks
//!
//! Requires Deployment Id extension to available

use serde_json::value::RawValue;
use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Path, Request},
    middleware::Next,
    response::Response,
    RequestExt,
};
use indexer_common::error::IndexerError;
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;

use crate::tap::AgoraQuery;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct QueryBody {
    query: String,
    variables: Option<Box<RawValue>>,
}

pub async fn context_middleware(
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerError> {
    let deployment_id = match request.extensions().get::<DeploymentId>() {
        Some(deployment) => *deployment,
        None => match request.extract_parts::<Path<DeploymentId>>().await {
            Ok(Path(deployment)) => deployment,
            Err(_) => return Err(IndexerError::DeploymentIdNotFound),
        },
    };

    let (mut parts, body) = request.into_parts();
    let bytes = to_bytes(body, usize::MAX).await?;
    let query_body: QueryBody = serde_json::from_slice(&bytes)?;

    let variables = query_body
        .variables
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();

    let mut ctx = Context::new();
    ctx.insert(AgoraQuery {
        deployment_id,
        query: query_body.query.clone(),
        variables,
    });
    parts.extensions.insert(Arc::new(ctx));
    let request = Request::from_parts(parts, bytes.into());
    Ok(next.run(request).await)
}
