//! injects the attestation signer

use std::collections::HashMap;

use alloy::primitives::Address;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use tokio::sync::watch;

use crate::{attestations::signer::AttestationSigner, error::IndexerError};

use super::allocation::Allocation;

#[derive(Clone)]
pub struct MyState {
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
    State(state): State<MyState>,
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerError> {
    if let Some(Allocation(allocation_id)) = request.extensions().get::<Allocation>() {
        if let Some(signer) = state.attestation_signers.borrow().get(allocation_id) {
            request.extensions_mut().insert(signer.clone());
        }
    }

    Ok(next.run(request).await)
}
