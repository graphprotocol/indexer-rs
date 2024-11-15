//! injects signed receipt into extensions

use super::TapReceipt;
use axum::{extract::Request, middleware::Next, response::Response, RequestExt};
use axum_extra::TypedHeader;

pub async fn receipt_middleware(mut request: Request, next: Next) -> Response {
    if let Ok(TypedHeader(receipt)) = request.extract_parts::<TypedHeader<TapReceipt>>().await {
        request
            .extensions_mut()
            .insert(receipt.into_signed_receipt());
    }
    next.run(request).await
}
