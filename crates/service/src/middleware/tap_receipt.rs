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
    // First check if header exists to distinguish missing vs invalid
    let has_tap_header = request.headers().contains_key("tap-receipt");

    match request.extract_parts::<TypedHeader<TapHeader>>().await {
        Ok(TypedHeader(TapHeader(receipt))) => {
            tracing::debug!("TAP receipt extracted successfully");
            request.extensions_mut().insert(receipt);
        }
        Err(e) => {
            if has_tap_header {
                tracing::error!(error = %e, "TAP receipt header present but invalid");
            } else {
                tracing::warn!("TAP receipt header missing (likely free query)");
            }
        }
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
    use base64::prelude::*;
    use prost::Message;
    use reqwest::StatusCode;
    use tap_aggregator::grpc::v2::SignedReceipt;
    use test_assets::create_signed_receipt_v2;
    use tower::ServiceExt;

    use crate::{middleware::tap_receipt::receipt_middleware, service::TapHeader, tap::TapReceipt};

    #[tokio::test]
    async fn test_receipt_middleware() {
        let middleware = from_fn(receipt_middleware);

        let receipt = create_signed_receipt_v2().call().await;
        let protobuf_receipt = SignedReceipt::from(receipt.clone());
        let encoded = protobuf_receipt.encode_to_vec();
        let receipt_b64 = BASE64_STANDARD.encode(encoded);

        let receipt = TapReceipt(receipt);

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
                    .header(TapHeader::name(), receipt_b64)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
