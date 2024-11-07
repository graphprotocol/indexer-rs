use axum::Json;
use serde_json::{json, Value};

use crate::error::SubgraphServiceError;

use super::subgraph_service::AttestationOutput;

#[derive(Debug)]
pub struct SubgraphServiceResponse {
    inner: String,
    attestable: bool,
}

impl SubgraphServiceResponse {
    pub fn new(inner: String, attestable: bool) -> Self {
        Self { inner, attestable }
    }

    pub fn is_attestable(&self) -> bool {
        self.attestable
    }

    pub fn as_str(&self) -> Result<&str, SubgraphServiceError> {
        Ok(self.inner.as_str())
    }

    pub fn finalize(self, attestation: AttestationOutput) -> Json<Value> {
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
