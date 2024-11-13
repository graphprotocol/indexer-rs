use axum::Json;
use indexer_common::attestations::{AttestableResponse, AttestationOutput};
use serde_json::{json, Value};

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

impl AttestableResponse<Json<Value>> for SubgraphServiceResponse {
    fn finalize(self, attestation: AttestationOutput) -> Json<Value> {
        let (attestation_key, attestation_value) = match attestation {
            AttestationOutput::Attestation(attestation) => ("attestation", json!(attestation)),
            AttestationOutput::Attestable => ("attestable", json!(self.is_attestable())),
        };
        Json(json!({
            "graphQLResponse": self.inner,
            attestation_key: attestation_value,
        }))
    }

    fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    fn is_attestable(&self) -> bool {
        self.attestable
    }
}
