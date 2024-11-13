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
    type Future = Ready<Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>;

    fn authorize(&mut self, request: Request<ReqBody>) -> Self::Future {
        let res = match request.headers().get(header::AUTHORIZATION) {
            Some(actual) if actual == self.header_value => Ok(Request::new(request.into_body())),
            _ => {
                let mut res = Response::default();
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                Err(res)
            }
        };
        ready(res)
    }
}

pub trait OrExt<T, B>: Sized {
    fn or(self, other: T) -> Or<Self, T>;
}

impl<T, A, B> OrExt<A, B> for T
where
    T: AsyncAuthorizeRequest<B>,
    A: AsyncAuthorizeRequest<B>,
{
    fn or(self, other: A) -> Or<Self, A> {
        Or(self, other)
    }
}

#[derive(Clone)]
pub struct Or<T, E>(T, E);

impl<T, E, B, Req, Resp> AsyncAuthorizeRequest<B> for Or<T, E>
where
    T: AsyncAuthorizeRequest<B, RequestBody = Req, ResponseBody = Resp>,
    E: AsyncAuthorizeRequest<B, RequestBody = Req, ResponseBody = Resp>,
    B: Clone + 'static,
{
    type RequestBody = Req;
    type ResponseBody = Resp;

    type Future = OrFuture<T::Future, E::Future>;

    fn authorize(&mut self, request: axum::http::Request<B>) -> Self::Future {
        OrFuture {
            fut_1_solved: false,
            fut_1: self.0.authorize(request.clone()),
            fut_2: self.1.authorize(request),
        }
    }
}

#[pin_project]
pub struct OrFuture<F1, F2> {
    #[pin]
    fut_1: F1,

    #[pin]
    fut_2: F2,

    fut_1_solved: bool,
}

impl<F1, F2, Resp, E> Future for OrFuture<F1, F2>
where
    F1: Future<Output = Result<Request<Resp>, Response<E>>>,
    F2: Future<Output = Result<Request<Resp>, Response<E>>>,
{
    type Output = Result<Request<Resp>, Response<E>>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();

        if !*this.fut_1_solved {
            if let Poll::Ready(res) = this.fut_1.poll(cx) {
                *this.fut_1_solved = true;
                if res.is_ok() {
                    Poll::Ready(res)
                } else {
                    this.fut_2.poll(cx)
                }
            } else {
                Poll::Pending
            }
        } else {
            this.fut_2.poll(cx)
        }
    }
}
