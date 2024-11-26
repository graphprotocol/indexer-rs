// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::Request, middleware::Next, response::Response, RequestExt};
use axum_extra::TypedHeader;

use crate::service::TapReceipt;

/// Injects tap receipts in the extensions
///
/// A request won't always have a receipt because they might be
/// free queries.
/// That's why we don't fail with 400.
///
/// This is useful to not deserialize multiple times the same receipt
pub async fn receipt_middleware(mut request: Request, next: Next) -> Response {
    if let Ok(TypedHeader(TapReceipt(receipt))) =
        request.extract_parts::<TypedHeader<TapReceipt>>().await
    {
        request.extensions_mut().insert(receipt);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use crate::{middleware::tap_receipt::receipt_middleware, service::TapReceipt};

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
    use test_assets::{create_signed_receipt, SignedReceiptRequest};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_receipt_middleware() {
        let middleware = from_fn(receipt_middleware);

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        let receipt_json = serde_json::to_string(&receipt).unwrap();

        let handle = move |extensions: Extensions| async move {
            let received_receipt = extensions
                .get::<SignedReceipt>()
                .expect("Should decode tap receipt");
            assert_eq!(received_receipt.message, receipt.message);
            assert_eq!(received_receipt.signature, receipt.signature);
            Body::empty()
        };

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(TapReceipt::name(), receipt_json)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
