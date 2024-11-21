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
