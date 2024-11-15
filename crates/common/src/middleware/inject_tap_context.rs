use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Path, Request},
    middleware::Next,
    response::Response,
    RequestExt,
};
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;

use crate::{error::IndexerError, tap::AgoraQuery};

use super::auth::QueryBody;

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
