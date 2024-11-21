// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Validates Tap receipts
//!
//! This looks for a Context in the extensions of the request to inject
//! as part of the checks.
//!
//! This also uses MetricLabels injected to

use std::{future::Future, sync::Arc};

use axum::{
    body::Body,
    http::{Request, Response},
    response::IntoResponse,
};
use tap_core::{
    manager::{adapters::ReceiptStore, Manager},
    receipt::{Context, SignedReceipt},
};
use tower_http::auth::AsyncAuthorizeRequest;

use crate::{error::IndexerServiceError, middleware::metrics::MetricLabels};

pub fn tap_receipt_authorize<T, B>(
    tap_manager: &'static Manager<T>,
    failed_receipt_metric: &'static prometheus::CounterVec,
) -> impl AsyncAuthorizeRequest<
    B,
    RequestBody = B,
    ResponseBody = Body,
    Future = impl Future<Output = Result<Request<B>, Response<Body>>> + Send,
> + Clone
       + Send
where
    T: ReceiptStore + Sync + Send,
    B: Send,
{
    |request: Request<B>| {
        let receipt = request.extensions().get::<SignedReceipt>().cloned();
        // load labels from previous middlewares
        let labels = request.extensions().get::<MetricLabels>().cloned();
        // load context from previous middlewares
        let ctx = request.extensions().get::<Arc<Context>>().cloned();

        async {
            let execute = || async {
                let receipt = receipt.ok_or(IndexerServiceError::ReceiptNotFound)?;

                // Verify the receipt and store it in the database
                tap_manager
                    .verify_and_store_receipt(&ctx.unwrap_or_default(), receipt)
                    .await
                    .inspect_err(|_| {
                        if let Some(labels) = labels {
                            failed_receipt_metric
                                .with_label_values(&labels.get_labels())
                                .inc()
                        }
                    })?;
                Ok::<_, IndexerServiceError>(request)
            };
            execute().await.map_err(|error| error.into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use alloy::primitives::{address, Address};
    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use prometheus::core::Collector;
    use reqwest::StatusCode;
    use sqlx::PgPool;
    use tap_core::{
        manager::Manager,
        receipt::{
            checks::{Check, CheckError, CheckList, CheckResult},
            state::Checking,
            ReceiptWithState,
        },
    };
    use test_assets::{create_signed_receipt, TAP_EIP712_DOMAIN};
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_http::auth::AsyncRequireAuthorizationLayer;

    use crate::{
        middleware::{
            auth::tap_receipt_authorize,
            metrics::{MetricLabelProvider, MetricLabels},
        },
        tap::IndexerTapContext,
    };

    const ALLOCATION_ID: Address = address!("deadbeefcafebabedeadbeefcafebabedeadbeef");

    async fn handle(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Ok(Response::new(Body::default()))
    }

    struct TestLabel;
    impl MetricLabelProvider for TestLabel {
        fn get_labels(&self) -> Vec<&str> {
            vec!["label1"]
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_tap_middleware(pgpool: PgPool) {
        let context = IndexerTapContext::new(pgpool.clone(), TAP_EIP712_DOMAIN.clone()).await;

        struct MyCheck;
        #[async_trait::async_trait]
        impl Check for MyCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                receipt: &ReceiptWithState<Checking>,
            ) -> CheckResult {
                if receipt.signed_receipt().message.nonce == 99 {
                    Err(CheckError::Failed(anyhow::anyhow!("Failed")))
                } else {
                    Ok(())
                }
            }
        }

        let tap_manager = Box::leak(Box::new(Manager::new(
            TAP_EIP712_DOMAIN.clone(),
            context,
            CheckList::new(vec![Arc::new(MyCheck)]),
        )));
        let metric = Box::leak(Box::new(
            prometheus::register_counter_vec!(
                "test1",
                "Failed queries to handler",
                &["deployment"]
            )
            .unwrap(),
        ));

        let tap_auth = tap_receipt_authorize(tap_manager, metric);

        let authorization_middleware = AsyncRequireAuthorizationLayer::new(tap_auth);

        let mut service = ServiceBuilder::new()
            .layer(authorization_middleware)
            .service_fn(handle);

        let handle = service.ready().await.unwrap();

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
        // if it fails tap receipt, should return failed to process payment + tap message

        assert_eq!(metric.collect().first().unwrap().get_metric().len(), 0);

        // default labels, all empty
        let labels: MetricLabels = Arc::new(TestLabel);

        let mut receipt = create_signed_receipt(ALLOCATION_ID, 1, 1, 1).await;
        // change the nonce to make the receipt invalid
        receipt.message.nonce = 99;
        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(receipt);
        req.extensions_mut().insert(labels);
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        assert_eq!(metric.collect().first().unwrap().get_metric().len(), 1);

        // if it doesnt contain the signed receipt
        // should return payment required
        let req = Request::new(Default::default());
        let res = handle.call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
    }
}
