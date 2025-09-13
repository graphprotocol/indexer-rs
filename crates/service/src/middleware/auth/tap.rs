// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Validates Tap receipts
//!
//! This looks for a Context in the extensions of the request to inject
//! as part of the checks.
//!
//! This also uses MetricLabels injected in the receipts to provide
//! metrics related to receipt check failure

use std::{future::Future, sync::Arc};

use axum::{
    body::Body,
    http::{Request, Response},
    response::IntoResponse,
};
use tap_core::{
    manager::{adapters::ReceiptStore, Manager},
    receipt::Context,
};
use tower_http::auth::AsyncAuthorizeRequest;

use crate::{
    error::IndexerServiceError, middleware::prometheus_metrics::MetricLabels, tap::TapReceipt,
};

/// Middleware to verify and store TAP receipts
///
/// It also optionally updates a failed receipt metric if Labels are provided
///
/// Requires TapReceipt, MetricLabels and Arc<Context> extensions
#[allow(dead_code)]
// keep this code as reference only
pub fn tap_receipt_authorize<T, B>(
    tap_manager: Arc<Manager<T, TapReceipt>>,
    failed_receipt_metric: &'static prometheus::CounterVec,
) -> impl AsyncAuthorizeRequest<
    B,
    RequestBody = B,
    ResponseBody = Body,
    Future = impl Future<Output = Result<Request<B>, Response<Body>>> + Send,
> + Clone
       + Send
where
    T: ReceiptStore<TapReceipt> + Sync + Send + 'static,
    B: Send,
{
    move |mut request: Request<B>| {
        let receipt = request.extensions_mut().remove::<TapReceipt>();
        // load labels from previous middlewares
        let labels = request.extensions().get::<MetricLabels>().cloned();
        // load context from previous middlewares
        let ctx = request.extensions().get::<Arc<Context>>().cloned();
        let tap_manager = tap_manager.clone();

        async move {
            let execute = || async {
                let receipt = receipt.ok_or_else(|| {
                    tracing::debug!(
                        "TAP receipt validation failed: receipt not found in request extensions"
                    );
                    IndexerServiceError::ReceiptNotFound
                })?;

                let version = match &receipt {
                    TapReceipt::V1(_) => "V1",
                    TapReceipt::V2(_) => "V2",
                };
                tracing::debug!(receipt_version = version, "Starting TAP receipt validation");

                // Verify the receipt and store it in the database
                tap_manager
                    .verify_and_store_receipt(&ctx.unwrap_or_default(), receipt)
                    .await
                    .inspect_err(|err| {
                        tracing::debug!(error = %err, receipt_version = version, "TAP receipt validation failed");
                        if let Some(labels) = &labels {
                            failed_receipt_metric
                                .with_label_values(&labels.get_labels())
                                .inc()
                        }
                    })?;

                tracing::debug!(
                    receipt_version = version,
                    "TAP receipt validation successful"
                );
                Ok::<_, IndexerServiceError>(request)
            };
            execute().await.map_err(|error| {
                tracing::debug!(error = %error, "TAP authorization failed, returning HTTP error response");
                error.into_response()
            })
        }
    }
}

pub fn dual_tap_receipt_authorize<T, B>(
    tap_manager_v1: Arc<Manager<T, TapReceipt>>,
    tap_manager_v2: Arc<Manager<T, TapReceipt>>,
    failed_receipt_metric: &'static prometheus::CounterVec,
) -> impl AsyncAuthorizeRequest<
    B,
    RequestBody = B,
    ResponseBody = Body,
    Future = impl Future<Output = Result<Request<B>, Response<Body>>> + Send,
> + Clone
       + Send
where
    T: ReceiptStore<TapReceipt> + Sync + Send + 'static,
    B: Send,
{
    move |mut request: Request<B>| {
        let receipt = request.extensions_mut().remove::<TapReceipt>();
        let labels = request.extensions().get::<MetricLabels>().cloned();
        let ctx = request.extensions().get::<Arc<Context>>().cloned();
        let manager_v1 = tap_manager_v1.clone();
        let manager_v2 = tap_manager_v2.clone();

        async move {
            let execute = || async {
                let receipt = receipt.ok_or_else(|| {
                    tracing::debug!(
                        "TAP receipt validation failed: receipt not found in request extensions"
                    );
                    IndexerServiceError::ReceiptNotFound
                })?;

                // SELECT THE RIGHT MANAGER BASED ON RECEIPT VERSION
                let (tap_manager, version) = match &receipt {
                    TapReceipt::V1(_) => (manager_v1, "V1"),
                    TapReceipt::V2(_) => (manager_v2, "V2"),
                };

                tracing::debug!(receipt_version = version, "Using version-specific manager");

                // Use the version-appropriate manager
                tap_manager
                    .verify_and_store_receipt(&ctx.unwrap_or_default(), receipt)
                    .await
                    .inspect_err(|err| {
                        tracing::debug!(error = %err, receipt_version = version, "TAP receipt validation failed");
                        if let Some(labels) = &labels {
                            failed_receipt_metric
                                .with_label_values(&labels.get_labels())
                                .inc()
                        }
                    })?;

                tracing::debug!(
                    receipt_version = version,
                    "TAP receipt validation successful"
                );
                Ok::<_, IndexerServiceError>(request)
            };
            execute().await.map_err(|error| {
                tracing::debug!(error = %error, "TAP authorization failed, returning HTTP error response");
                error.into_response()
            })
        }
    }
}

#[cfg(test)]
mod tests {

    use core::panic;
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use prometheus::core::Collector;
    use reqwest::StatusCode;
    use rstest::*;
    use sqlx::PgPool;
    use tap_core::{
        manager::Manager,
        receipt::checks::{Check, CheckError, CheckList, CheckResult},
    };
    use test_assets::{
        assert_while_retry, create_signed_receipt, SignedReceiptRequest, TAP_EIP712_DOMAIN,
        TAP_EIP712_DOMAIN_V2,
    };
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_http::auth::AsyncRequireAuthorizationLayer;

    use crate::{
        middleware::prometheus_metrics::{MetricLabelProvider, MetricLabels},
        tap::{CheckingReceipt, IndexerTapContext, TapReceipt},
    };

    use super::tap_receipt_authorize;

    #[fixture]
    fn metric() -> &'static prometheus::CounterVec {
        let registry = prometheus::Registry::new();
        let metric = Box::leak(Box::new(
            prometheus::register_counter_vec_with_registry!(
                "tap_middleware_test",
                "Failed queries to handler",
                &["deployment"],
                registry,
            )
            .unwrap(),
        ));
        metric
    }

    const FAILED_NONCE: u64 = 99;

    async fn service(
        metric: &'static prometheus::CounterVec,
        pgpool: PgPool,
    ) -> impl Service<Request<Body>, Response = Response<Body>, Error = impl std::fmt::Debug> {
        let context = IndexerTapContext::new(
            pgpool,
            TAP_EIP712_DOMAIN.clone(),
            TAP_EIP712_DOMAIN_V2.clone(),
        )
        .await;

        struct MyCheck;
        #[async_trait::async_trait]
        impl Check<TapReceipt> for MyCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                receipt: &CheckingReceipt,
            ) -> CheckResult {
                if receipt.signed_receipt().nonce() == FAILED_NONCE {
                    Err(CheckError::Failed(anyhow::anyhow!("Failed")))
                } else {
                    Ok(())
                }
            }
        }

        let manager = Arc::new(Manager::new(
            TAP_EIP712_DOMAIN.clone(),
            context,
            CheckList::new(vec![Arc::new(MyCheck)]),
        ));
        let tap_auth = tap_receipt_authorize(manager, metric);
        let authorization_middleware = AsyncRequireAuthorizationLayer::new(tap_auth);

        let mut service = ServiceBuilder::new()
            .layer(authorization_middleware)
            .service_fn(|_: Request<Body>| async {
                Ok::<_, anyhow::Error>(Response::new(Body::default()))
            });

        service.ready().await.unwrap();
        service
    }

    #[rstest]
    #[tokio::test]
    async fn test_tap_valid_receipt(metric: &'static prometheus::CounterVec) {
        let test_db = test_assets::setup_shared_test_db().await;
        let mut service = service(metric, test_db.pool.clone()).await;

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;

        // check with receipt
        let mut req = Request::new(Body::default());
        req.extensions_mut().insert(TapReceipt::V1(receipt));
        let res = service.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // verify receipts
        assert_while_retry!({
            sqlx::query!("SELECT * FROM scalar_tap_receipts")
                .fetch_all(&test_db.pool)
                .await
                .unwrap()
                .is_empty()
        })
    }

    #[rstest]
    #[tokio::test]
    async fn test_invalid_receipt_with_failed_metric(metric: &'static prometheus::CounterVec) {
        let test_db = test_assets::setup_shared_test_db().await;
        let mut service = service(metric, test_db.pool.clone()).await;
        // if it fails tap receipt, should return failed to process payment + tap message

        assert_eq!(metric.collect().first().unwrap().get_metric().len(), 0);

        struct TestLabel;
        impl MetricLabelProvider for TestLabel {
            fn get_labels(&self) -> Vec<&str> {
                vec!["label1"]
            }
        }

        // default labels, all empty
        let labels: MetricLabels = Arc::new(TestLabel);

        let mut receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        // change the nonce to make the receipt invalid
        receipt.message.nonce = FAILED_NONCE;
        let mut req = Request::new(Body::default());
        req.extensions_mut().insert(TapReceipt::V1(receipt));
        req.extensions_mut().insert(labels);
        let response = service.call(req);

        assert_eq!(response.await.unwrap().status(), StatusCode::BAD_REQUEST);

        assert_eq!(metric.collect().first().unwrap().get_metric().len(), 1);
    }

    #[rstest]
    #[tokio::test]
    async fn test_tap_missing_signed_receipt(metric: &'static prometheus::CounterVec) {
        let test_db = test_assets::setup_shared_test_db().await;
        let mut service = service(metric, test_db.pool.clone()).await;
        // if it doesnt contain the signed receipt
        // should return payment required
        let req = Request::new(Body::default());
        let res = service.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
    }
}
