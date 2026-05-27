// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Catches errors from inner tower layers and turns them into gRPC
//! error responses.
//!
//! Tonic's `Server::add_service` requires its routed service to declare
//! `Error = Infallible` — a promise that the service never fails. Tower
//! layers like `RateLimit`, `Timeout`, the per-IP `GovernorLayer`, and
//! `Buffer` all return real errors, so wrapping them around tonic's
//! `Routes` would break that promise.
//!
//! This wrapper sits outside the inner stack, intercepts any inner
//! error, and emits a successful HTTP response carrying an appropriate
//! gRPC status code. The wrapper's own error type is `Infallible`,
//! restoring tonic's expected bound.

use std::{
    convert::Infallible,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use axum::http::{Request, Response};
use pin_project::pin_project;
use tonic::Status;
use tower::{BoxError, Layer, Service};

#[derive(Clone, Copy, Debug, Default)]
pub struct GrpcErrorToResponseLayer;

impl<S> Layer<S> for GrpcErrorToResponseLayer {
    type Service = GrpcErrorToResponse<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcErrorToResponse { inner }
    }
}

#[derive(Clone, Debug)]
pub struct GrpcErrorToResponse<S> {
    inner: S,
}

impl<S, ReqBody, RespBody> Service<Request<ReqBody>> for GrpcErrorToResponse<S>
where
    S: Service<Request<ReqBody>, Response = Response<RespBody>>,
    S::Error: Into<BoxError>,
    RespBody: Default,
{
    type Response = Response<RespBody>;
    type Error = Infallible;
    type Future = GrpcErrorToResponseFuture<S::Future, RespBody>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.inner.poll_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            // The inner stack's `poll_ready` should not error in practice
            // — buffer/timeout/rate-limit/governor surface errors through
            // the response future, not through readiness. If it does, log
            // and report ready; the next `call` will return an error that
            // we then map to a status.
            Poll::Ready(Err(e)) => {
                let boxed: BoxError = e.into();
                tracing::error!(
                    error = %boxed,
                    "DIPs middleware poll_ready returned an error; reporting ready and deferring to call"
                );
                Poll::Ready(Ok(()))
            }
        }
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        GrpcErrorToResponseFuture {
            inner: self.inner.call(req),
            _body: PhantomData,
        }
    }
}

#[pin_project]
pub struct GrpcErrorToResponseFuture<F, B> {
    #[pin]
    inner: F,
    _body: PhantomData<B>,
}

impl<F, E, B> Future for GrpcErrorToResponseFuture<F, B>
where
    F: Future<Output = Result<Response<B>, E>>,
    E: Into<BoxError>,
    B: Default,
{
    type Output = Result<Response<B>, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.inner.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(resp)) => Poll::Ready(Ok(resp)),
            Poll::Ready(Err(e)) => {
                let boxed: BoxError = e.into();
                let status = classify(&*boxed);
                tracing::warn!(
                    error = %boxed,
                    code = ?status.code(),
                    "DIPs middleware mapped inner error to gRPC status"
                );
                Poll::Ready(Ok(status.into_http()))
            }
        }
    }
}

/// Walk the error chain looking for known inner-stack failures and
/// map each to the closest matching gRPC status code. Falls back to
/// `Internal` when nothing matches, which mirrors tonic's default for
/// unhandled service failures.
fn classify(e: &(dyn std::error::Error + 'static)) -> Status {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = current {
        if err
            .downcast_ref::<tower::timeout::error::Elapsed>()
            .is_some()
        {
            return Status::deadline_exceeded("request exceeded the DIPs handler timeout");
        }
        if err
            .downcast_ref::<tower_governor::errors::GovernorError>()
            .is_some()
        {
            return Status::resource_exhausted("per-IP rate limit exceeded");
        }
        if err.downcast_ref::<tower::buffer::error::Closed>().is_some() {
            return Status::unavailable("DIPs handler queue closed");
        }
        current = err.source();
    }
    Status::internal("DIPs handler internal error")
}
