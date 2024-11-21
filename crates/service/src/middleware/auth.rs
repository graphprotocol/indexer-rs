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
    use std::time::Duration;

    use alloy::primitives::{address, Address};
    use axum::body::Body;
    use axum::http::{Request, Response};
    use reqwest::{header, StatusCode};
    use sqlx::PgPool;
    use tap_core::{manager::Manager, receipt::checks::CheckList};
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_http::auth::AsyncRequireAuthorizationLayer;

    use crate::middleware::auth::{self, Bearer, OrExt};
    use crate::tap::IndexerTapContext;
    use test_assets::{create_signed_receipt, TAP_EIP712_DOMAIN};

    const ALLOCATION_ID: Address = address!("deadbeefcafebabedeadbeefcafebabedeadbeef");

    async fn handle(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Ok(Response::new(Body::default()))
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_middleware_composition(pgpool: PgPool) {
        let token = "test".to_string();
        let context = IndexerTapContext::new(pgpool.clone(), TAP_EIP712_DOMAIN.clone()).await;
        let tap_manager = Box::leak(Box::new(Manager::new(
            TAP_EIP712_DOMAIN.clone(),
            context,
            CheckList::empty(),
        )));
        let metric = Box::leak(Box::new(
            prometheus::register_counter_vec!(
                "merge_checks_test",
                "Failed queries to handler",
                &["deployment"]
            )
            .unwrap(),
        ));
        let free_query = Bearer::new(&token);
        let tap_auth = auth::tap_receipt_authorize(tap_manager, metric);
        let authorize_requests = free_query.or(tap_auth);

        let authorization_middleware = AsyncRequireAuthorizationLayer::new(authorize_requests);

        let mut service = ServiceBuilder::new()
            .layer(authorization_middleware)
            .service_fn(handle);

        let handle = service.ready().await.unwrap();

        // should allow queries that contains the free token
        // if the token does not match, return payment required
        let mut req = Request::new(Default::default());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // if the token exists but is wrong, try the receipt
        let mut req = Request::new(Default::default());
        req.headers_mut()
            .insert(header::AUTHORIZATION, "Bearer wrongtoken".parse().unwrap());
        let res = handle.call(req).await.unwrap();
        // we return the error from tap
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);

        let receipt = create_signed_receipt(ALLOCATION_ID, 1, 1, 1).await;

        // check with receipt
        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(receipt);
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // todo make this sleep better
        tokio::time::sleep(Duration::from_millis(100)).await;

        // verify receipts
        let result = sqlx::query!("SELECT * FROM scalar_tap_receipts")
            .fetch_all(&pgpool)
            .await
            .unwrap();
        assert_eq!(result.len(), 1);

        // if it has neither, should return unauthorized
        // check no headers
        let req = Request::new(Default::default());
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
    }
}
