//! injects the attestation signer
//!
//! Needs Allocation Extension to be added

use std::collections::HashMap;

use alloy::primitives::Address;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use indexer_common::attestations::signer::AttestationSigner;
use tokio::sync::watch;

use super::allocation::Allocation;

#[derive(Clone)]
pub struct AttestationState {
    pub attestation_signers: watch::Receiver<HashMap<Address, AttestationSigner>>,
}

#[derive(Clone)]
pub struct Sender(String);

impl From<Sender> for String {
    fn from(value: Sender) -> Self {
        value.0
    }
}

pub async fn signer_middleware(
    State(state): State<AttestationState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(Allocation(allocation_id)) = request.extensions().get::<Allocation>() {
        if let Some(signer) = state.attestation_signers.borrow().get(allocation_id) {
            request.extensions_mut().insert(signer.clone());
        }
    }

    next.run(request).await
}
