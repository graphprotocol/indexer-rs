// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Path, Request},
    middleware::Next,
    response::Response,
    RequestExt,
};
use thegraph_core::DeploymentId;

/// Injects deployment id in the extensions from the path
pub async fn deployment_middleware(mut request: Request, next: Next) -> Response {
    let deployment_id = request.extract_parts::<Path<DeploymentId>>().await.ok();
    if let Some(Path(deployment_id)) = deployment_id {
        request.extensions_mut().insert(deployment_id);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use test_assets::ESCROW_SUBGRAPH_DEPLOYMENT;
    use thegraph_core::DeploymentId;
    use tower::ServiceExt;

    use super::deployment_middleware;

    #[tokio::test]
    async fn test_deployment_middleware() {
        let middleware = from_fn(deployment_middleware);

        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;

        let handle = move |extensions: Extensions| async move {
            let received_deployment = extensions
                .get::<DeploymentId>()
                .expect("Should contain a deployment_id");
            assert_eq!(*received_deployment, deployment);
            Body::empty()
        };

        let app = Router::new()
            .route("/{deployment_id}", get(handle))
            .layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{deployment}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
