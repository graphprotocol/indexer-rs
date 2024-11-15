pub mod attestation;
pub mod auth;
pub mod metrics;
pub mod tap_header;

pub use tap_header::TapReceipt;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use alloy::primitives::{address, Address};
    use axum::body::Body;
    use axum::http::{Request, Response};
    use prometheus::core::Collector;
    use reqwest::{header, StatusCode};
    use sqlx::PgPool;
    use tap_core::{manager::Manager, receipt::checks::CheckList};
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_http::auth::AsyncRequireAuthorizationLayer;

    use crate::middleware::auth::{self, Bearer, OrExt};
    use crate::test_vectors::{NETWORK_SUBGRAPH_DEPLOYMENT, TAP_EIP712_DOMAIN};
    use crate::{
        middleware::metrics::MetricLabels, tap::IndexerTapContext,
        test_vectors::create_signed_receipt,
    };

    use super::metrics::{MetricLabelProvider, MetricsMiddlewareLayer};

    const ALLOCATION_ID: Address = address!("deadbeefcafebabedeadbeefcafebabedeadbeef");

    async fn handle(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Ok(Response::new(Body::default()))
    }

    async fn handle_err(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Err(anyhow::anyhow!("Error"))
    }

    #[sqlx::test(migrations = "../migrations")]
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
                "test1",
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
        // if the token does not match, return unauthorized
        let mut req = Request::new(Default::default());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let mut req = Request::new(Default::default());
        req.headers_mut()
            .insert(header::AUTHORIZATION, "Bearer wrongtoken".parse().unwrap());
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let receipt = create_signed_receipt(ALLOCATION_ID, 1, 1, 1).await;

        // check with receipt
        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(receipt);
        req.extensions_mut().insert(*NETWORK_SUBGRAPH_DEPLOYMENT);
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
        // if it fails tap receipt, should return failed to process payment + tap message

        // if it has neither, should return unauthorized
        // check no headers
        let req = Request::new(Default::default());
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // if the query don't contains token, should verify and store tap TapReceipt
    }
    struct TestLabel;
    impl MetricLabelProvider for TestLabel {
        fn get_labels(&self) -> Vec<&str> {
            vec!["label1,", "label2", "label3"]
        }
    }

    #[tokio::test]
    async fn test_metrics_middleware() {
        let registry = prometheus::Registry::new();
        let histogram_metric = prometheus::register_histogram_vec_with_registry!(
            "histogram_metric",
            "Test",
            &["deployment", "sender", "allocation"],
            registry,
        )
        .unwrap();

        let failure_metric = prometheus::register_counter_vec_with_registry!(
            "failure_metric",
            "Test",
            &["deployment", "sender", "allocation"],
            registry,
        )
        .unwrap();

        // check if everything is clean
        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            0
        );
        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            0
        );

        let metrics_layer =
            MetricsMiddlewareLayer::new(histogram_metric.clone(), failure_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle);
        let handle = service.ready().await.unwrap();

        // default labels, all empty
        let labels: MetricLabels = Arc::new(TestLabel);

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels.clone());
        let _ = handle.call(req).await;

        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            1
        );

        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            0
        );

        let metrics_layer =
            MetricsMiddlewareLayer::new(histogram_metric.clone(), failure_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle_err);
        let handle = service.ready().await.unwrap();

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels);
        let _ = handle.call(req).await;

        // it's using the same labels, should have only one metric
        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            1
        );

        // new failture
        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            1
        );
    }

    // #[test]
    // fn test_inject_allocation() {
    //     let inject_allocation = AllocationMiddleware::new();
    // }
}
