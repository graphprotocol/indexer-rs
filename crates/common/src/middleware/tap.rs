//! verifies and stores tap receipt, or else reject

use std::{future::Future, marker::PhantomData, task::Poll};

use super::metrics::MetricLabels;
use crate::tap::AgoraQuery;
use axum::{
    body::{to_bytes, Body},
    http::{Request, Response},
};
use pin_project::pin_project;
use serde_json::value::RawValue;
use tap_core::{
    manager::{adapters::ReceiptStore, Manager},
    receipt::{Context, SignedReceipt},
};
use thegraph_core::DeploymentId;
use tower_http::auth::AsyncAuthorizeRequest;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct QueryBody {
    pub query: String,
    pub variables: Option<Box<RawValue>>,
}

pub struct TapReceiptAuthorize<F1, F2, Func, Func2> {
    failed_receipt_metric: prometheus::CounterVec,
    func: Option<Func>,
    func2: Option<Func2>,
    _ty: PhantomData<fn(F1) -> F2>,
}

impl<F1, F2, Func, Func2> Clone for TapReceiptAuthorize<F1, F2, Func, Func2> {
    fn clone(&self) -> Self {
        todo!()
        // Self {
        //     failed_receipt_metric: self.failed_receipt_metric.clone(),
        //     func: self.func.clone(),
        //     func2: self.func2.clone(),
        //     _ty: self._ty.clone(),
        // }
    }
}

pub fn tap_receipt_authorize<T>(
    tap_manager: Manager<T>,
    failed_receipt_metric: prometheus::CounterVec,
) -> impl AsyncAuthorizeRequest<
    axum::body::Body,
    RequestBody = Body,
    ResponseBody = Body,
    Future = impl Send,
> + Clone
       + Send
where
    T: ReceiptStore + Send + Sync,
{
    TapReceiptAuthorize {
        // tap_manager,
        failed_receipt_metric,
        func: Some(move |ctx: Context, receipt: SignedReceipt| async { todo!() }),
        func2: Some(|body| to_bytes(body, usize::MAX)),
        _ty: PhantomData,
    }
}

// impl<T, F1, F2, Func, Func2> TapReceiptAuthorize<T, F1, F2, Func, Func2> {
//     pub fn new(
//         tap_manager: Arc<Manager<T>>,
//         failed_receipt_metric: prometheus::CounterVec,
//     ) -> Self {
//         Self {
//             tap_manager,
//             failed_receipt_metric,
//             _ty: PhantomData,
//         }
//     }
// }

impl<F1, F2, Func, Func2> AsyncAuthorizeRequest<Body> for TapReceiptAuthorize<F1, F2, Func, Func2>
where
    Func: FnOnce(Context, SignedReceipt) -> F2,
    Func2: FnOnce(Body) -> F1,
    F1: Future<Output = Result<axum::body::Bytes, axum::Error>>,
    F2: Future<Output = Result<(), tap_core::Error>>,
{
    type RequestBody = Body;
    type ResponseBody = Body;

    type Future = TapFuture<F1, F2, Func>;
    // type Future = Pin<
    //     Box<
    //         dyn Future<Output = Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>
    //             + Send,
    //     >,
    // >;

    fn authorize(&mut self, request: Request<Body>) -> Self::Future {
        let receipt = match request.extensions().get::<SignedReceipt>() {
            Some(receipt) => receipt.clone(),
            None => {
                // TODO extract from header
                return TapFuture::error(); //Err(anyhow!("Could not find receipt"));
            }
        };
        let labels = request.extensions().get::<MetricLabels>().cloned();

        let deployment_id = match request.extensions().get::<DeploymentId>() {
            Some(deployment) => *deployment,
            None => {
                // TODO extract from path
                return TapFuture::error(); //  Err(anyhow!("Could not find deployment id"));
            }
        };

        let body = request.into_body();
        let fut = (self.func2.take().unwrap())(body);

        TapFuture {
            state: State::ConvertToBytes {
                fut,
                func: Some(self.func.take().unwrap()),
                signed_receipt: receipt,
                deployment_id,
            },
        }
        // let this = self.clone();
        // Box::pin(async move {
        //     let execute = move || async move {
        //         let receipt = match request.extensions().get::<SignedReceipt>() {
        //             Some(receipt) => receipt.clone(),
        //             None => {
        //                 // TODO extract from header
        //                 return Err(anyhow!("Could not find receipt"));
        //             }
        //         };
        //         let labels = request.extensions().get::<MetricLabels>().cloned();
        //
        //         let deployment_id = match request.extensions().get::<DeploymentId>() {
        //             Some(deployment) => *deployment,
        //             None => {
        //                 // TODO extract from path
        //                 return Err(anyhow!("Could not find deployment id"));
        //             }
        //         };
        //
        //         let body = request.into_body();
        //         let bytes = to_bytes(body, usize::MAX).await?;
        //         let query_body: QueryBody = serde_json::from_slice(&bytes)?;
        //
        //         let variables = query_body
        //             .variables
        //             .as_ref()
        //             .map(ToString::to_string)
        //             .unwrap_or_default();
        //
        //         let mut ctx = Context::new();
        //         ctx.insert(AgoraQuery {
        //             deployment_id,
        //             query: query_body.query.clone(),
        //             variables,
        //         });
        //
        //         // Verify the receipt and store it in the database
        //         this.tap_manager
        //             .verify_and_store_receipt(&ctx, receipt)
        //             .await
        //             .inspect_err(|_| {
        //                 if let Some(labels) = labels {
        //                     this.failed_receipt_metric
        //                         .with_label_values(&labels.get_labels())
        //                         .inc()
        //                 }
        //             })?;
        //
        //         Ok::<_, anyhow::Error>(Request::new(bytes.into()))
        //     };
        //     execute().await.map_err(|e| {
        //         let mut res = Response::new(Default::default());
        //         *res.status_mut() = StatusCode::UNAUTHORIZED;
        //         res
        //     })
        // })
    }
}

#[pin_project::pin_project(project = KindProj)]
enum State<F1, F2, Func> {
    ConvertToBytes {
        #[pin]
        fut: F1,

        func: Option<Func>,
        signed_receipt: SignedReceipt,
        deployment_id: DeploymentId,
    },
    VerifyAndStore {
        #[pin]
        fut: F2,
        bytes: axum::body::Bytes,
    },
    Done {
        body: Option<Request<Body>>,
    },
    Error {
        error: String,
    },
}

#[pin_project]
pub struct TapFuture<F1, F2, Func> {
    #[pin]
    state: State<F1, F2, Func>,
    // _resp: std::marker::PhantomData<fn(Req) -> Res>,
}

impl<F1, F2, Func> TapFuture<F1, F2, Func> {
    fn error() -> Self {
        Self {
            state: State::Error {
                error: "".to_string(),
            },
        }
    }
}

impl<F1, F2, Func> Future for TapFuture<F1, F2, Func>
where
    F1: Future<Output = Result<axum::body::Bytes, axum::Error>>,
    Func: FnOnce(Context, SignedReceipt) -> F2,
    F2: Future<Output = Result<(), tap_core::Error>>,
{
    type Output = Result<Request<Body>, Response<Body>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.state.as_mut().project() {
                // KindProj::Initial { request, signed_receipt } => todo!(),
                KindProj::ConvertToBytes {
                    fut,
                    signed_receipt,
                    func,
                    deployment_id,
                } => match fut.poll(cx) {
                    Poll::Ready(result) => match result {
                        Ok(bytes) => {
                            let Ok(query_body) = serde_json::from_slice::<QueryBody>(&bytes) else {
                                this.state.set(State::Error {
                                    error: "error".to_string(),
                                });
                                continue;
                            };

                            let variables = query_body
                                .variables
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_default();

                            let mut ctx = Context::new();
                            ctx.insert(AgoraQuery {
                                deployment_id: *deployment_id,
                                query: query_body.query.clone(),
                                variables,
                            });
                            let fut = (func.take().unwrap())(ctx, signed_receipt.clone());
                            this.state.set(State::VerifyAndStore { fut, bytes });
                        }
                        Err(_) => {
                            this.state.set(State::Error {
                                error: "error".to_string(),
                            });
                            continue;
                        }
                    },
                    Poll::Pending => return Poll::Pending,
                },
                KindProj::VerifyAndStore { fut, bytes } => match fut.poll(cx) {
                    Poll::Ready(v) => match v {
                        Ok(_) => {
                            let bytes: axum::body::Body = bytes.clone().into();
                            this.state.set(State::Done {
                                body: Some(Request::new(bytes)),
                            });
                        }
                        Err(_) => {
                            this.state.set(State::Error {
                                error: "error".to_string(),
                            });
                            continue;
                        }
                    },
                    Poll::Pending => return Poll::Pending,
                },
                KindProj::Done { body } => return Poll::Ready(Ok(body.take().unwrap())),
                KindProj::Error { error } => return Poll::Ready(Err(Response::new(Body::empty()))),
            }
        }
    }
}
