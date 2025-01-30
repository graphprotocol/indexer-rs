// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::Request, middleware::Next, response::Response, RequestExt};
use axum_extra::TypedHeader;

use crate::service::TapHeader;

/// Injects tap receipts in the extensions
///
/// A request won't always have a receipt because they might be
/// free queries.
/// That's why we don't fail with 400.
///
/// This is useful to not deserialize multiple times the same receipt
pub async fn receipt_middleware(mut request: Request, next: Next) -> Response {
    if let Ok(TypedHeader(TapHeader(receipt))) =
        request.extract_parts::<TypedHeader<TapHeader>>().await
    {
        request.extensions_mut().insert(receipt);
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
    use axum_extra::headers::Header;
    use reqwest::StatusCode;
    use test_assets::{create_signed_receipt, SignedReceiptRequest};
    use tower::ServiceExt;

    use crate::{middleware::tap_receipt::receipt_middleware, service::TapHeader, tap::TapReceipt};

    #[tokio::test]
    async fn test_receipt_middleware() {
        let middleware = from_fn(receipt_middleware);

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        let receipt_json = serde_json::to_string(&receipt).unwrap();

        let receipt = TapReceipt::V1(receipt);

        let handle = move |extensions: Extensions| async move {
            let received_receipt = extensions
                .get::<TapReceipt>()
                .expect("Should decode tap receipt");
            assert_eq!(*received_receipt, receipt);
            Body::empty()
        };

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(TapHeader::name(), receipt_json)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
