// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use indexer_attestation::AttestationSigner;
use thegraph_core::alloy::primitives::Address;
use tokio::sync::watch;

use super::Allocation;

#[derive(Clone)]
pub struct AttestationState {
    pub attestation_signers: watch::Receiver<HashMap<Address, AttestationSigner>>,
}

/// Injects the attestation signer to be used in the attestation
///
/// Needs Allocation Extension
pub async fn signer_middleware(
    State(state): State<AttestationState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(Allocation(allocation_id)) = request.extensions().get::<Allocation>() {
        if let Some(signer) = state.attestation_signers.borrow().get(allocation_id) {
            request.extensions_mut().insert(signer.clone());
        } else {
            // Just log this case which is silently passed through next middleware
            tracing::warn!(
                "No attestation signer found for allocation {}",
                allocation_id
            );
        }
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request, middleware::from_fn_with_state, routing::get, Router};
    use indexer_attestation::AttestationSigner;
    use indexer_monitor::attestation_signers;
    use reqwest::StatusCode;
    use test_assets::{DISPUTE_MANAGER_ADDRESS, INDEXER_ALLOCATIONS, INDEXER_MNEMONIC};
    use tokio::sync::{mpsc::channel, watch};
    use tower::Service;

    use crate::middleware::{allocation::Allocation, signer_middleware, AttestationState};

    #[tokio::test]
    async fn test_attestation_signer_middleware() {
        let allocations = (*INDEXER_ALLOCATIONS).clone();

        let allocation = **allocations.keys().collect::<Vec<_>>().first().unwrap();

        let (_, allocations_rx) = watch::channel(allocations);
        let (_, dispute_manager_rx) = watch::channel(DISPUTE_MANAGER_ADDRESS);
        let attestation_signers = attestation_signers(
            allocations_rx,
            INDEXER_MNEMONIC.clone(),
            1,
            dispute_manager_rx,
        );

        let expected_signer = attestation_signers
            .borrow()
            .get(&allocation)
            .unwrap()
            .clone();

        let state = AttestationState {
            attestation_signers,
        };

        let middleware = from_fn_with_state(state, signer_middleware);

        let (tx, mut rx) = channel(1);

        let handle = move |request: Request<Body>| async move {
            tx.send(request).await.unwrap();
            Body::empty()
        };

        let mut app = Router::new().route("/", get(handle)).layer(middleware);

        // with allocation
        let res = app
            .call(
                Request::builder()
                    .uri("/")
                    .extension(Allocation(allocation))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = rx.recv().await.unwrap();
        let signer = req.extensions().get::<AttestationSigner>().unwrap();
        assert_eq!(*signer, expected_signer);

        // without allocation
        let res = app
            .call(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = rx.recv().await.unwrap();
        assert!(req.extensions().get::<AttestationSigner>().is_none());
    }
}
