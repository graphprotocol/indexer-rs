// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::Request, middleware::Next, response::Response, RequestExt};
use axum_extra::TypedHeader;

use crate::service::TapReceipt;

pub async fn receipt_middleware(mut request: Request, next: Next) -> Response {
    if let Ok(TypedHeader(receipt)) = request.extract_parts::<TypedHeader<TapReceipt>>().await {
        if let Some(receipt) = receipt.into_signed_receipt() {
            request.extensions_mut().insert(receipt);
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use crate::{middleware::inject_receipt::receipt_middleware, service::TapReceipt};

    use alloy::primitives::Address;
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use axum_extra::headers::Header;
    use reqwest::StatusCode;
    use tap_core::receipt::SignedReceipt;
    use test_assets::create_signed_receipt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_receipt_middleware() {
        let middleware = from_fn(receipt_middleware);

        async fn handle(extensions: Extensions) -> Body {
            extensions
                .get::<SignedReceipt>()
                .expect("Should decode tap receipt");
            Body::empty()
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let receipt = create_signed_receipt(Address::ZERO, 1, 1, 1).await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(TapReceipt::name(), serde_json::to_string(&receipt).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
