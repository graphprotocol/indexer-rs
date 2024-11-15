//! check if the query can be attestable generates attestation
//! executes query -> return subgraph response: (string, attestable (bool))
//! if attestable && allocation id:
//!     - look for signer
//!     - create attestation
//!     - return response with attestation
//! else:
//!     - return with no attestation
//!
//! Required AttestationSigner

use std::string::FromUtf8Error;

use axum::{
    body::to_bytes,
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use reqwest::StatusCode;
use serde::Serialize;
use thegraph_core::Attestation;

use crate::attestations::signer::AttestationSigner;

#[derive(Clone)]
pub enum AttestationInput {
    Attestable { req: String, res: String },
    NotAttestable,
}

#[derive(Debug, Serialize)]
pub struct IndexerResponsePayload {
    #[serde(rename = "graphQLResponse")]
    graphql_response: String,
    attestation: Option<Attestation>,
}

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
    let graphql_response = String::from_utf8(bytes.into())?;

    let attestation = match attestation_response {
        Some(AttestationInput::Attestable { req, res }) => {
            Some(signer.create_attestation(req, res))
        }
        _ => None,
    };

    Ok(Response::new(
        serde_json::to_string(&IndexerResponsePayload {
            graphql_response,
            attestation,
        })?
        .into(),
    ))
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

impl IntoResponse for AttestationError {
    fn into_response(self) -> Response {
        match self {
            AttestationError::CouldNotFindSigner
            | AttestationError::AxumError(_)
            | AttestationError::FromUtf8Error(_)
            | AttestationError::SerializationError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
        .into_response()
    }
}
