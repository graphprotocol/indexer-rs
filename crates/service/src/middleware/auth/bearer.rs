// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Bearer struct from tower-http but exposing the `new()` function
//! to allow creation
//!
//! This code is from *tower-http*

use std::{fmt, marker::PhantomData};

use axum::http::{HeaderValue, Request, Response};
use reqwest::{header, StatusCode};
use tower_http::validate_request::ValidateRequest;

pub struct Bearer<ResBody> {
    header_value: HeaderValue,
    _ty: PhantomData<fn() -> ResBody>,
}

impl<ResBody> Bearer<ResBody> {
    pub fn new(token: &str) -> Self
    where
        ResBody: Default,
    {
        Self {
            header_value: format!("Bearer {token}")
                .parse()
                .expect("token is not a valid header value"),
            _ty: PhantomData,
        }
    }
}

impl<ResBody> Clone for Bearer<ResBody> {
    fn clone(&self) -> Self {
        Self {
            header_value: self.header_value.clone(),
            _ty: PhantomData,
        }
    }
}

impl<ResBody> fmt::Debug for Bearer<ResBody> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bearer")
            .field("header_value", &self.header_value)
            .finish()
    }
}

impl<B, ResBody> ValidateRequest<B> for Bearer<ResBody>
where
    ResBody: Default,
{
    type ResponseBody = ResBody;

    fn validate(&mut self, request: &mut Request<B>) -> Result<(), Response<Self::ResponseBody>> {
        match request.headers().get(header::AUTHORIZATION) {
            Some(actual) if actual == self.header_value => Ok(()),
            _ => {
                let mut res = Response::new(ResBody::default());
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                Err(res)
            }
        }
    }
}
