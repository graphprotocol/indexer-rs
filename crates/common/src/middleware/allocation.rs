//! injects allocation id in extensions
//! - check if allocation id already exists
//! - else, try to fetch allocation id from deployment_id and allocations watcher
//! - execute query

use std::collections::HashMap;

use alloy::primitives::Address;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use tap_core::receipt::SignedReceipt;
use thegraph_core::DeploymentId;
use tokio::sync::watch;

#[derive(Clone)]
pub struct Allocation(pub Address);

impl From<Allocation> for String {
    fn from(value: Allocation) -> Self {
        value.0.to_string()
    }
}

pub struct MyState {
    attestation: watch::Receiver<HashMap<DeploymentId, Address>>,
}

pub async fn allocation_middleware(
    State(my_state): State<MyState>,
    mut request: Request,
    next: Next,
) -> Result<Response, anyhow::Error> {
    if let Some(receipt) = request.extensions().get::<SignedReceipt>() {
        let allocation = receipt.message.allocation_id;
        request.extensions_mut().insert(Allocation(allocation));
    } else if let Some(deployment_id) = request.extensions().get::<DeploymentId>() {
        if let Some(allocation) = my_state.attestation.borrow().get(deployment_id) {
            request.extensions_mut().insert(Allocation(*allocation));
        }
    }

    Ok(next.run(request).await)
}
