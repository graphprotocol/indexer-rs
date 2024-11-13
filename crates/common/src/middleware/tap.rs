//! verifies and stores tap receipt, or else reject

use std::{future::Future, marker::PhantomData, pin::Pin, sync::Arc};

use super::metrics::MetricLabels;
use crate::tap::AgoraQuery;
use anyhow::anyhow;
use axum::{
    body::{to_bytes, Body},
    extract::{Path, Request},
    http::Response,
    RequestExt,
};
use reqwest::StatusCode;
use serde_json::value::RawValue;
use tap_core::{
    manager::{adapters::ReceiptStore, Manager},
    receipt::{Context, SignedReceipt},
};
use thegraph_core::DeploymentId;
use tower_http::auth::AsyncAuthorizeRequest;

#[derive(Debug, serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub struct QueryBody {
    pub query: String,
    pub variables: Option<Box<RawValue>>,
}

#[derive(Clone)]
pub struct TapReceiptAuthorize<T> {
    tap_manager: Arc<Manager<T>>,
    failed_receipt_metric: prometheus::CounterVec,
}

impl<T> TapReceiptAuthorize<T> {
    pub fn new(
        tap_manager: Arc<Manager<T>>,
        failed_receipt_metric: prometheus::CounterVec,
    ) -> Self {
        Self {
            tap_manager,
            failed_receipt_metric,
        }
    }
}

impl<T> AsyncAuthorizeRequest<Body> for TapReceiptAuthorize<T>
where
    T: Clone + ReceiptStore + 'static,
{
    type RequestBody = Body;

    type ResponseBody = String;

    // type Future = TapFuture<Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>;
    type Future = Pin<
        Box<dyn Future<Output = Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>>,
    >;

    fn authorize(&mut self, mut request: Request) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            let execute = move || async move {
                let receipt = match request.extensions().get::<SignedReceipt>() {
                    Some(receipt) => receipt.clone(),
                    None => {
                        // extract from header
                        todo!()
                    }
                };
                let labels = request.extensions().get::<MetricLabels>().cloned();

                let deployment_id = match request.extensions().get::<DeploymentId>() {
                    Some(deployment) => *deployment,
                    None => {
                        // extract from path
                        if let Ok(Path(deployment)) =
                            request.extract_parts::<Path<DeploymentId>>().await
                        {
                            deployment
                        } else {
                            return Err(anyhow!("Could not find deployment_id"));
                        }
                    }
                };

                let (_, body) = request.into_parts();
                let bytes = to_bytes(body, usize::MAX).await?;
                let query_body: QueryBody = serde_json::from_slice(&bytes)?;

                let variables = query_body
                    .variables
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let mut ctx = Context::new();
                ctx.insert(AgoraQuery {
                    deployment_id,
                    query: query_body.query.clone(),
                    variables,
                });

                // Verify the receipt and store it in the database
                this.tap_manager
                    .verify_and_store_receipt(&ctx, receipt)
                    .await
                    .inspect_err(|_| {
                        if let Some(labels) = labels {
                            this.failed_receipt_metric
                                .with_label_values(&labels.get_labels())
                                .inc()
                        }
                    })?;
                Ok::<_, anyhow::Error>(Request::new(bytes.into()))
            };
            execute().await.map_err(|_| {
                let mut res = Response::new(String::default());
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                res
            })
        })
    }
}

pub struct TapFuture<T> {
    _t: PhantomData<T>,
}

impl<Req, Resp> Future for TapFuture<Result<Request<Req>, Response<Resp>>> {
    type Output = Result<Request<Req>, Response<Resp>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        todo!()
    }
}
