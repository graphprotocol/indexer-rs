// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Merge a ValidateRequest and an AsyncAuthorizeRequest
//!
//! executes a ValidateRequest returning the request if it succeeds
//! or else, executes the future and return it

use std::marker::PhantomData;

use axum::http::{request::Parts, Request, Response};
use thegraph_core::alloy::transports::BoxFuture;
use tower_http::auth::AsyncAuthorizeRequest;

use super::async_validate::AsyncAuthorizeRequestExt;

/// Extension that allows using a simple .or() function and return an Or struct
pub trait OrExt<T, B, Resp>: Sized {
    fn or(self, other: T) -> Or<Self, T, B, Resp>;
}

impl<T, A, B, Resp> OrExt<A, B, Resp> for T
where
    B: 'static + Send,
    Resp: 'static + Send,
    T: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send,
    A: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send,
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

impl<T, E, B, Resp> AsyncAuthorizeRequestExt for Or<T, E, B, Resp>
where
    T: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send + Sync,
    E: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send + Sync,
{
    type ResponseBody = Resp;

    async fn authorize(&self, parts: &mut Parts) -> Result<(), Response<Resp>> {
        if self.0.authorize(parts).await.is_err() {
            self.1.authorize(parts).await?;
        }
        Ok(())
    }
}

impl<T, E, Req, Resp> AsyncAuthorizeRequest<Req> for Or<T, E, Req, Resp>
where
    Req: 'static + Send,
    Resp: 'static + Send,
    T: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send,
    E: AsyncAuthorizeRequestExt<ResponseBody = Resp> + Clone + 'static + Send,
{
    type RequestBody = Req;
    type ResponseBody = Resp;

    type Future = BoxFuture<'static, Result<Request<Req>, Response<Resp>>>;

    fn authorize(&mut self, req: axum::http::Request<Req>) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            let (mut parts, body) = req.into_parts();
            if this.0.authorize(&mut parts).await.is_err() {
                this.1.authorize(&mut parts).await?;
            }
            let req = Request::from_parts(parts, body);
            Ok(req)
        })
    }
}
