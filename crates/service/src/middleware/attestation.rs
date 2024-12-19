// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::string::FromUtf8Error;

use axum::{
    body::to_bytes,
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use indexer_attestation::AttestationSigner;
use reqwest::StatusCode;
use serde::Serialize;
use thegraph_core::attestation::Attestation;

use crate::error::StatusCodeExt;

#[derive(Clone)]
pub enum AttestationInput {
    Attestable { req: String },
    NotAttestable,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct IndexerResponsePayload {
    #[serde(rename = "graphQLResponse")]
    graphql_response: String,
    attestation: Option<Attestation>,
}

/// Check if the query is attestable and generates attestation
///
/// Executes query -> return subgraph response: (string, attestable (bool))
/// if attestable && allocation id:
///     - look for signer
///     - create attestation
///     - return response with attestation
/// else:
///     - return with no attestation
///
/// Requires AttestationSigner
pub async fn attestation_middleware(
    request: Request,
    next: Next,
) -> Result<Response, AttestationError> {
    let signer = request
        .extensions()
        .get::<AttestationSigner>()
        .cloned()
        .ok_or(AttestationError::CouldNotFindSigner)?;

    let (parts, graphql_response) = next.run(request).await.into_parts();
    let attestation_response = parts.extensions.get::<AttestationInput>();
    let bytes = to_bytes(graphql_response, usize::MAX).await?;
    let res = String::from_utf8(bytes.into())?;

    let attestation = match attestation_response {
        Some(AttestationInput::Attestable { req }) => Some(signer.create_attestation(req, &res)),
        _ => None,
    };

    let response = serde_json::to_string(&IndexerResponsePayload {
        graphql_response: res,
        attestation,
    })?;

    Ok(Response::new(response.into()))
}

#[derive(thiserror::Error, Debug)]
pub enum AttestationError {
    #[error("Could not find signer for allocation")]
    CouldNotFindSigner,

    #[error("There was an AxumError: {0}")]
    AxumError(#[from] axum::Error),

    #[error("There was an error converting the response to UTF-8 string: {0}")]
    FromUtf8Error(#[from] FromUtf8Error),

    #[error("there was an error while serializing the response: {0}")]
    SerializationError(#[from] serde_json::Error),
}

impl StatusCodeExt for AttestationError {
    fn status_code(&self) -> StatusCode {
        match self {
            AttestationError::CouldNotFindSigner => StatusCode::INTERNAL_SERVER_ERROR,
            AttestationError::AxumError(_)
            | AttestationError::FromUtf8Error(_)
            | AttestationError::SerializationError(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for AttestationError {
    fn into_response(self) -> Response {
        self.status_code().into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, Response},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use indexer_allocation::Allocation;
    use indexer_attestation::AttestationSigner;
    use reqwest::StatusCode;
    use test_assets::{INDEXER_ALLOCATIONS, INDEXER_MNEMONIC};
    use thegraph_core::alloy::primitives::Address;
    use tower::ServiceExt;

    use crate::middleware::{
        attestation::IndexerResponsePayload, attestation_middleware, AttestationInput,
    };

    const REQUEST: &str = "request";
    const RESPONSE: &str = "response";

    fn allocation_signer() -> (Allocation, AttestationSigner) {
        let allocation = INDEXER_ALLOCATIONS
            .values()
            .collect::<Vec<_>>()
            .pop()
            .unwrap()
            .clone();
        let signer =
            AttestationSigner::new(&INDEXER_MNEMONIC.to_string(), &allocation, 1, Address::ZERO)
                .unwrap();
        (allocation, signer)
    }

    async fn payload_from_response(res: Response<Body>) -> IndexerResponsePayload {
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send_request(app: Router, signer: Option<AttestationSigner>) -> Response<Body> {
        let mut request = Request::builder().uri("/");

        if let Some(signer) = signer {
            request = request.extension(signer);
        }

        app.oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_attestation() {
        let (allocation, signer) = allocation_signer();
        let middleware = from_fn(attestation_middleware);

        let handle = move |_: Request<Body>| async move {
            let mut res = Response::new(RESPONSE.to_string());
            res.extensions_mut().insert(AttestationInput::Attestable {
                req: REQUEST.to_string(),
            });
            res
        };

        let app = Router::new().route("/", get(handle)).layer(middleware);

        // with signer
        let res = send_request(app, Some(signer.clone())).await;
        assert_eq!(res.status(), StatusCode::OK);

        let response = payload_from_response(res).await;
        assert_eq!(response.graphql_response, RESPONSE.to_string());

        let attestation = response.attestation.unwrap();
        assert!(signer
            .verify(&attestation, REQUEST, RESPONSE, &allocation.id)
            .is_ok());
    }

    #[tokio::test]
    async fn test_non_assignable() {
        let (_, signer) = allocation_signer();
        let handle = move |_: Request<Body>| async move { Response::new(RESPONSE.to_string()) };

        let middleware = from_fn(attestation_middleware);
        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = send_request(app, Some(signer.clone())).await;
        assert_eq!(res.status(), StatusCode::OK);

        let response = payload_from_response(res).await;
        assert_eq!(response.graphql_response, RESPONSE.to_string());
        assert!(response.attestation.is_none());
    }

    #[tokio::test]
    async fn test_no_signer() {
        let handle = move |_: Request<Body>| async move {
            Response::new(RESPONSE.to_string());
        };

        let middleware = from_fn(attestation_middleware);
        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = send_request(app, None).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
