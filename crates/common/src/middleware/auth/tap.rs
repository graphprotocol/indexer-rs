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
    RequestExt,
};
use axum_extra::TypedHeader;
use reqwest::StatusCode;
use serde_json::value::RawValue;
use tap_core::{
    manager::{adapters::ReceiptStore, Manager},
    receipt::{Context, SignedReceipt},
};
use tower_http::auth::AsyncAuthorizeRequest;

use crate::middleware::{metrics::MetricLabels, TapReceipt};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct QueryBody {
    pub query: String,
    pub variables: Option<Box<RawValue>>,
}

pub fn tap_receipt_authorize<T>(
    tap_manager: &'static Manager<T>,
    failed_receipt_metric: &'static prometheus::CounterVec,
) -> impl AsyncAuthorizeRequest<
    Body,
    RequestBody = Body,
    ResponseBody = Body,
    Future = impl Future<Output = Result<Request<Body>, Response<Body>>> + Send,
> + Clone
       + Send
where
    T: ReceiptStore + Sync + Send,
{
    |mut request: Request<Body>| {
        let receipt = request.extensions().get::<SignedReceipt>().cloned();
        // load labels from previous middlewares
        let labels = request.extensions().get::<MetricLabels>().cloned();
        // load context from previous middlewares
        let ctx = request.extensions().get::<Arc<Context>>().cloned();

        async {
            let execute = || async {
                let receipt = match receipt {
                    Some(receipt) => Ok(receipt),
                    None => {
                        // if we don't have it provided, try to extract from header
                        if let Ok(TypedHeader(receipt)) =
                            request.extract_parts::<TypedHeader<TapReceipt>>().await
                        {
                            Ok(receipt.into_signed_receipt())
                        } else {
                            Err(anyhow::anyhow!("Could not find receipt"))
                        }
                    }
                }?;

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
                Ok::<_, anyhow::Error>(request)
            };
            execute().await.map_err(|_| {
                let mut res = Response::new(Body::default());
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                res
            })
        }
    }
}
