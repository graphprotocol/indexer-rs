use std::future::Future;

use axum::{
    body::Body,
    http::{request::Parts, Request, Response},
};
use tower_http::auth::AsyncAuthorizeRequest;

pub trait AsyncAuthorizeRequestExt {
    /// The body type used for responses to unauthorized requests.
    type ResponseBody;

    /// Authorize the request.
    ///
    /// If the future resolves to `Ok(request)` then the request is allowed through, otherwise not.
    fn authorize(
        &self,
        request: &mut Parts,
    ) -> impl Future<Output = Result<(), Response<Self::ResponseBody>>> + Send;
}

//) -> impl AsyncAuthorizeRequest<
//    B,
//    RequestBody = B,
//    ResponseBody = Body,
//    Future = impl Future<Output = Result<Request<B>, Response<Body>>> + Send,
//> + Clone
//       + Send
//where
//    T: ReceiptStore + Sync + Send + 'static,
//    B: Send,

pub fn wrap<B>(
    my_fn: impl AsyncAuthorizeRequestExt<ResponseBody = Body> + Clone + Send,
) -> impl AsyncAuthorizeRequest<
    B,
    RequestBody = B,
    ResponseBody = Body,
    Future = impl Future<Output = Result<Request<B>, Response<Body>>> + Send,
> + Clone
       + Send
where
    B: Send,
{
    move |request: Request<B>| {
        let my_fn = my_fn.clone();
        async move {
            let (mut parts, body) = request.into_parts();
            my_fn.authorize(&mut parts).await?;
            let request = Request::from_parts(parts, body);
            Ok(request)
        }
    }
}

impl<F, Fut, ResBody> AsyncAuthorizeRequestExt for F
where
    F: Fn(&mut Parts) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), Response<ResBody>>> + Send,
{
    type ResponseBody = ResBody;

    async fn authorize(&self, request: &mut Parts) -> Result<(), Response<ResBody>> {
        self(request).await
    }
}
