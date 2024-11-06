use axum::{response::IntoResponse, Json};
use serde_json::{json, Value};

use crate::error::SubgraphServiceError;

use super::subgraph_service::AttestationOutput;


pub trait IndexerServiceResponse {
    type Data: IntoResponse;
    type Error: std::error::Error;

    fn is_attestable(&self) -> bool;
    fn as_str(&self) -> Result<&str, Self::Error>;
    fn finalize(self, attestation: AttestationOutput) -> Self::Data;
}


#[derive(Debug)]
pub struct SubgraphServiceResponse {
    inner: String,
    attestable: bool,
}

impl SubgraphServiceResponse {
    pub fn new(inner: String, attestable: bool) -> Self {
        Self { inner, attestable }
    }
}

impl IndexerServiceResponse for SubgraphServiceResponse {
    type Data = Json<Value>;
    type Error = SubgraphServiceError; // not used

    fn is_attestable(&self) -> bool {
        self.attestable
    }

    fn as_str(&self) -> Result<&str, Self::Error> {
        Ok(self.inner.as_str())
    }

    fn finalize(self, attestation: AttestationOutput) -> Self::Data {
        let (attestation_key, attestation_value) = match attestation {
            AttestationOutput::Attestation(attestation) => ("attestation", json!(attestation)),
            AttestationOutput::Attestable => ("attestable", json!(self.is_attestable())),
        };
        Json(json!({
            "graphQLResponse": self.inner,
            attestation_key: attestation_value,
        }))
    }
}
