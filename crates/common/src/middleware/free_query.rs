//! validates if query contains free auth token

use std::{
    future::{ready, Future, Ready},
    marker::PhantomData,
    pin::Pin,
    task::Poll,
};

use axum::http::{Request, Response};
use headers::HeaderValue;
use pin_project::pin_project;
use reqwest::{header, StatusCode};
use tower_http::auth::AsyncAuthorizeRequest;

pub struct FreeQueryAuthorize<ReqBody, ResBody> {
    header_value: HeaderValue,
    _ty: PhantomData<fn(Request<ReqBody>) -> Response<ResBody>>,
}

impl<ReqBody, ResBody> Clone for FreeQueryAuthorize<ReqBody, ResBody> {
    fn clone(&self) -> Self {
        Self {
            header_value: self.header_value.clone(),
            _ty: self._ty,
        }
    }
}

impl<ReqBody, ResBody> FreeQueryAuthorize<ReqBody, ResBody> {
    pub fn new(token: String) -> Self {
        Self {
            header_value: format!("Bearer {}", token)
                .parse()
                .expect("token is not a valid header value"),
            _ty: PhantomData,
        }
    }
}

impl<ReqBody, ResBody> AsyncAuthorizeRequest<ReqBody> for FreeQueryAuthorize<ReqBody, ResBody>
where
    ResBody: Default,
{
    type RequestBody = ReqBody;
    type ResponseBody = ResBody;
    type Future = Ready<Result<Request<ReqBody>, Response<Self::ResponseBody>>>;

    fn authorize(&mut self, request: Request<ReqBody>) -> Self::Future {
        let res = match request.headers().get(header::AUTHORIZATION) {
            Some(actual) if actual == self.header_value => Ok(request),
            _ => {
                let mut res = Response::default();
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                Err(res)
            }
        };
        ready(res)
    }
}

impl<ReqBody, ResBody> AsyncAuthorizeRequestRef<ReqBody> for FreeQueryAuthorize<ReqBody, ResBody>
where
    ResBody: Default,
{
    type ResponseBody = ResBody;
    type Future = Ready<Result<(), Response<Self::ResponseBody>>>;

    fn authorize(&mut self, request: &Request<ReqBody>) -> Self::Future {
        let res = match request.headers().get(header::AUTHORIZATION) {
            Some(actual) if actual == self.header_value => Ok(()),
            _ => {
                let mut res = Response::default();
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                Err(res)
            }
        };
        ready(res)
    }
}

pub trait OrExt<T, B, Resp>: Sized
where
    Self: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>,
    T: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>,
{
    fn or(self, other: T) -> Or<Self, T, B, Resp>;
}

impl<T, A, B, Resp> OrExt<A, B, Resp> for T
where
    T: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>,
    A: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>,
{
    fn or(self, other: A) -> Or<Self, A, B, Resp> {
        Or(self, other, PhantomData)
    }
}

// #[derive(Clone)]
pub struct Or<T, E, B, Resp>(T, E, PhantomData<fn(B) -> Resp>)
where
    T: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>,
    E: AsyncAuthorizeRequestRef<B, ResponseBody = Resp>;

impl<T, E, B, Resp> Clone for Or<T, E, B, Resp>
where
    T: AsyncAuthorizeRequestRef<B, ResponseBody = Resp> + Clone,
    E: AsyncAuthorizeRequestRef<B, ResponseBody = Resp> + Clone,
{
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone(), self.2)
    }
}

impl<T, E, Req, Resp> AsyncAuthorizeRequest<Req> for Or<T, E, Req, Resp>
where
    T: AsyncAuthorizeRequestRef<Req, ResponseBody = Resp>,
    E: AsyncAuthorizeRequestRef<Req, ResponseBody = Resp>,
{
    type RequestBody = Req;
    type ResponseBody = Resp;

    type Future = OrFuture<T::Future, E::Future, Req>;

    fn authorize(&mut self, request: axum::http::Request<Req>) -> Self::Future {
        // todo!()
        OrFuture {
            fut_1_solved: false,
            fut_1: self.0.authorize(&request),
            fut_2: self.1.authorize(&request),
            request: Some(request),
        }
    }
}

#[pin_project]
pub struct OrFuture<F1, F2, B> {
    #[pin]
    fut_1: F1,

    #[pin]
    fut_2: F2,

    fut_1_solved: bool,
    request: Option<Request<B>>,
}

impl<F1, F2, Req, Resp> Future for OrFuture<F1, F2, Req>
where
    F1: Future<Output = Result<(), Response<Resp>>>,
    F2: Future<Output = Result<(), Response<Resp>>>,
{
    type Output = Result<Request<Req>, Response<Resp>>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();

        if !*this.fut_1_solved {
            if let Poll::Ready(res) = this.fut_1.poll(cx) {
                *this.fut_1_solved = true;
                if res.is_ok() {
                    Poll::Ready(Ok(this.request.take().unwrap()))
                } else {
                    match this.fut_2.poll(cx) {
                        Poll::Ready(res) => Poll::Ready(res.map(|_| this.request.take().unwrap())),
                        Poll::Pending => Poll::Pending,
                    }
                }
            } else {
                Poll::Pending
            }
        } else {
            match this.fut_2.poll(cx) {
                Poll::Ready(res) => Poll::Ready(res.map(|_| this.request.take().unwrap())),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

/// Trait for authorizing requests.
pub trait AsyncAuthorizeRequestRef<B> {
    /// The type of request body returned by `authorize`.

    /// The body type used for responses to unauthorized requests.
    type ResponseBody;

    /// The Future type returned by `authorize`
    type Future: Future<Output = Result<(), Response<Self::ResponseBody>>>;

    /// Authorize the request.
    ///
    /// If the future resolves to `Ok(request)` then the request is allowed through, otherwise not.
    fn authorize(&mut self, request: &Request<B>) -> Self::Future;
}

impl<B, F, Fut, ResBody> AsyncAuthorizeRequestRef<B> for F
where
    F: FnMut(&Request<B>) -> Fut,
    Fut: Future<Output = Result<(), Response<ResBody>>>,
{
    type ResponseBody = ResBody;
    type Future = Fut;

    fn authorize(&mut self, request: &Request<B>) -> Self::Future {
        self(request)
    }
}
