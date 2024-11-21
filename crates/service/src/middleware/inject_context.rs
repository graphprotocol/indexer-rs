// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

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
use tap_core::receipt::Context;
use thegraph_core::DeploymentId;

use crate::{error::IndexerServiceError, tap::AgoraQuery};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct QueryBody {
    query: String,
    variables: Option<Box<RawValue>>,
}

pub async fn context_middleware(
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerServiceError> {
    let deployment_id = match request.extensions().get::<DeploymentId>() {
        Some(deployment) => *deployment,
        None => match request.extract_parts::<Path<DeploymentId>>().await {
            Ok(Path(deployment)) => deployment,
            Err(_) => return Err(IndexerServiceError::DeploymentIdNotFound),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use tap_core::receipt::Context;
    use test_assets::ESCROW_SUBGRAPH_DEPLOYMENT;
    use tower::ServiceExt;

    use crate::{
        middleware::inject_context::{context_middleware, QueryBody},
        tap::AgoraQuery,
    };

    #[tokio::test]
    async fn test_context_middleware() {
        let middleware = from_fn(context_middleware);
        let deployment = *ESCROW_SUBGRAPH_DEPLOYMENT;
        let query_body = QueryBody {
            query: "hello".to_string(),
            variables: None,
        };
        let body = serde_json::to_string(&query_body).unwrap();

        let handle = move |extensions: Extensions| async move {
            let ctx = extensions
                .get::<Arc<Context>>()
                .expect("Should contain context");
            let agora = ctx.get::<AgoraQuery>().expect("should contain agora query");
            assert_eq!(agora.deployment_id, deployment);
            assert_eq!(agora.query, query_body.query);

            let variables = query_body
                .variables
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert_eq!(agora.variables, variables);
            Body::empty()
        };

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(deployment)
                    .extension(deployment)
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
