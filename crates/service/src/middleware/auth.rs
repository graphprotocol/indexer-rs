// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

mod bearer;
mod or;
mod tap;

pub use bearer::Bearer;
pub use or::OrExt;
pub use tap::tap_receipt_authorize;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use reqwest::{header, StatusCode};
    use sqlx::PgPool;
    use tap_core::{manager::Manager, receipt::checks::CheckList};
    use test_assets::{
        assert_while_retry, create_signed_receipt, SignedReceiptRequest, TAP_EIP712_DOMAIN,
    };
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_http::auth::AsyncRequireAuthorizationLayer;

    use crate::{
        middleware::auth::{self, Bearer, OrExt},
        tap_v1::IndexerTapContext,
    };

    const BEARER_TOKEN: &str = "test";

    async fn service(
        pgpool: PgPool,
    ) -> impl Service<Request<Body>, Response = Response<Body>, Error = impl std::fmt::Debug> {
        let context = IndexerTapContext::new(pgpool.clone(), TAP_EIP712_DOMAIN.clone()).await;
        let tap_manager = Arc::new(Manager::new(
            TAP_EIP712_DOMAIN.clone(),
            context,
            CheckList::empty(),
        ));

        let registry = prometheus::Registry::new();
        let metric = Box::leak(Box::new(
            prometheus::register_counter_vec_with_registry!(
                "merge_checks_test",
                "Failed queries to handler",
                &["deployment"],
                registry,
            )
            .unwrap(),
        ));
        let free_query = Bearer::new(BEARER_TOKEN);
        let tap_auth = auth::tap_receipt_authorize(tap_manager, metric);
        let authorize_requests = free_query.or(tap_auth);

        let authorization_middleware = AsyncRequireAuthorizationLayer::new(authorize_requests);

        let mut service = ServiceBuilder::new()
            .layer(authorization_middleware)
            .service_fn(|_: Request<Body>| async {
                Ok::<_, anyhow::Error>(Response::new(Body::default()))
            });

        service.ready().await.unwrap();
        service
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_composition_header_valid(pgpool: PgPool) {
        let mut service = service(pgpool.clone()).await;
        // should allow queries that contains the free token
        // if the token does not match, return payment required
        let mut req = Request::new(Default::default());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", BEARER_TOKEN).parse().unwrap(),
        );
        let res = service.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_composition_header_invalid(pgpool: PgPool) {
        let mut service = service(pgpool.clone()).await;

        // if the token exists but is wrong, try the receipt
        let mut req = Request::new(Default::default());
        req.headers_mut()
            .insert(header::AUTHORIZATION, "Bearer wrongtoken".parse().unwrap());
        let res = service.call(req).await.unwrap();
        // we return the error from tap
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_composition_with_receipt(pgpool: PgPool) {
        let mut service = service(pgpool.clone()).await;

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;

        // check with receipt
        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(receipt);
        let res = service.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // verify receipts
        assert_while_retry!({
            let result = sqlx::query!("SELECT * FROM scalar_tap_receipts")
                .fetch_all(&pgpool)
                .await
                .unwrap();
            result.is_empty()
        });
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_composition_without_header_or_receipt(pgpool: PgPool) {
        let mut service = service(pgpool.clone()).await;
        // if it has neither, should return payment required
        let req = Request::new(Default::default());
        let res = service.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
    }
}
