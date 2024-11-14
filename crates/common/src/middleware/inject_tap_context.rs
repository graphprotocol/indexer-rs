use std::sync::Arc;

use axum::{body::to_bytes, extract::Request, middleware::Next, response::Response};
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;

use crate::tap::AgoraQuery;

use super::tap::QueryBody;

pub async fn context_middleware(request: Request, next: Next) -> Result<Response, anyhow::Error> {
    let deployment_id = match request.extensions().get::<DeploymentId>() {
        Some(deployment) => *deployment,
        None => {
            // TODO extract from path
            return Err(anyhow::anyhow!("Could not find deployment id"));
        }
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
