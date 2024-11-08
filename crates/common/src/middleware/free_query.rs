use axum::{body::Body, extract::Request, response::Response};
use futures_util::future::BoxFuture;
use std::task::{Context, Poll};
use tower::{Layer, Service};

pub struct FreeQuery {
    token: String,
}

#[derive(Clone)]
struct MyLayer;

impl<S> Layer<S> for MyLayer {
    type Service = MyMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MyMiddleware { inner }
    }
}

#[derive(Clone)]
struct MyMiddleware<S> {
    inner: S,
    required_auth_token: String,
}

impl<S> Service<Request> for MyMiddleware<S>
where
    S: Service<Request, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let headers = request.headers();
        let authorization = headers
            .get("authorization")
            .map(|value| value.to_str())
            .transpose()?
            .ok_or_else(|| IndexerServiceError::Unauthorized)?
            .trim_start_matches("Bearer ");

        if authorization != self.required_auth_token {
            return Err(IndexerServiceError::Unauthorized);
        }

        self.inner.call(request)
    }
}
