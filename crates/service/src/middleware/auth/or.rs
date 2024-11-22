// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Merge a ValidateRequest and an AsyncAuthorizeRequest
//!
//! executes a ValidateRequest returning the request if it succeeds
//! or else, executed the future and return it

use std::{future::Future, marker::PhantomData, pin::Pin, task::Poll};

use axum::http::{Request, Response};
use pin_project::pin_project;
use tower_http::{auth::AsyncAuthorizeRequest, validate_request::ValidateRequest};

/// Extension that allows using a simple .or() function and return an Or struct
pub trait OrExt<T, B, Resp>: Sized {
    fn or(self, other: T) -> Or<Self, T, B, Resp>;
}

impl<T, A, B, Resp, Fut> OrExt<A, B, Resp> for T
where
    B: 'static + Send,
    Resp: 'static + Send,
    T: ValidateRequest<B, ResponseBody = Resp>,
    A: AsyncAuthorizeRequest<B, RequestBody = B, ResponseBody = Resp, Future = Fut>
        + Clone
        + 'static
        + Send,
    Fut: Future<Output = Result<Request<B>, Response<Resp>>> + Send,
{
    fn or(self, other: A) -> Or<Self, A, B, Resp> {
        Or(self, other, PhantomData)
    }
}

/// Or struct capable of implementing a ValidateRequest or an AsyncAuthorizeRequest
///
/// Uses the first parameter to validate the request sync.
/// if it passes the check return the request to pass to the next middleware
/// if it doesn't pass, check the async future returning the result
pub struct Or<T, E, B, Resp>(T, E, PhantomData<fn(B) -> Resp>);

impl<T, E, B, Resp> Clone for Or<T, E, B, Resp>
where
    T: Clone,
    E: Clone,
{
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone(), self.2)
    }
}

impl<T, E, Req, Resp, Fut> AsyncAuthorizeRequest<Req> for Or<T, E, Req, Resp>
where
    Req: 'static + Send,
    Resp: 'static + Send,
    T: ValidateRequest<Req, ResponseBody = Resp>,
    E: AsyncAuthorizeRequest<Req, RequestBody = Req, ResponseBody = Resp, Future = Fut>
        + Clone
        + 'static
        + Send,
    Fut: Future<Output = Result<Request<Req>, Response<Resp>>> + Send,
{
    type RequestBody = Req;
    type ResponseBody = Resp;

    type Future = OrFuture<Fut, Req, Resp>;

    fn authorize(&mut self, mut request: axum::http::Request<Req>) -> Self::Future {
        let mut this = self.1.clone();
        if self.0.validate(&mut request).is_ok() {
            return OrFuture::with_result(Ok(request));
        }
        OrFuture::with_future(this.authorize(request))
    }
}

#[pin_project::pin_project(project = KindProj)]
pub enum Kind<Fut, Req, Resp> {
    QueryResult {
        #[pin]
        fut: Fut,
    },
    ReturnResult {
        validation_result: Option<Result<Request<Req>, Response<Resp>>>,
    },
}

#[pin_project]
pub struct OrFuture<Fut, Req, Resp> {
    #[pin]
    kind: Kind<Fut, Req, Resp>,
}

impl<Fut, Req, Resp> OrFuture<Fut, Req, Resp> {
    fn with_result(validation_result: Result<Request<Req>, Response<Resp>>) -> Self {
        let validation_result = Some(validation_result);
        Self {
            kind: Kind::ReturnResult { validation_result },
        }
    }

    fn with_future(fut: Fut) -> Self {
        Self {
            kind: Kind::QueryResult { fut },
        }
    }
}

impl<Fut, Req, Resp> Future for OrFuture<Fut, Req, Resp>
where
    Fut: Future<Output = Result<Request<Req>, Response<Resp>>>,
{
    type Output = Result<Request<Req>, Response<Resp>>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.kind.project() {
            KindProj::QueryResult { fut } => fut.poll(cx),
            KindProj::ReturnResult { validation_result } => {
                Poll::Ready(validation_result.take().expect("cannot poll twice"))
            }
        }
    }
}
