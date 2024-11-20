// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Path, Request},
    middleware::Next,
    response::Response,
    RequestExt,
};
use thegraph_core::DeploymentId;

pub async fn deployment_middleware(mut request: Request, next: Next) -> Response {
    let deployment_id = request.extract_parts::<Path<DeploymentId>>().await.ok();
    if let Some(Path(deployment_id)) = deployment_id {
        request.extensions_mut().insert(deployment_id);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::deployment_middleware;
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

    #[tokio::test]
    async fn test_deployment_middleware() {
        let middleware = from_fn(deployment_middleware);

        async fn handle(extensions: Extensions) -> Body {
            extensions
                .get::<DeploymentId>()
                .expect("Should contain a deployment_id");
            Body::empty()
        }

        let app = Router::new()
            .route("/:deployment_id", get(handle))
            .layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{}", *ESCROW_SUBGRAPH_DEPLOYMENT))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
