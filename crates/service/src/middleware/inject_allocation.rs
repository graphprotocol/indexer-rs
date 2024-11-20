// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! injects allocation id in extensions
//! - check if allocation id already exists
//! - else, try to fetch allocation id from deployment_id and allocations watcher
//! - execute query
//!
//! Needs signed receipt Extension to be added OR deployment id

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

#[derive(Clone)]
pub struct AllocationState {
    pub deployment_to_allocation: watch::Receiver<HashMap<DeploymentId, Address>>,
}

pub async fn allocation_middleware(
    State(my_state): State<AllocationState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(receipt) = request.extensions().get::<SignedReceipt>() {
        let allocation = receipt.message.allocation_id;
        request.extensions_mut().insert(Allocation(allocation));
    } else if let Some(deployment_id) = request.extensions().get::<DeploymentId>() {
        if let Some(allocation) = my_state
            .deployment_to_allocation
            .borrow()
            .get(deployment_id)
        {
            request.extensions_mut().insert(Allocation(*allocation));
        }
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use crate::middleware::inject_allocation::Allocation;

    use super::{allocation_middleware, AllocationState};

    use alloy::primitives::Address;
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use test_assets::{create_signed_receipt, ESCROW_SUBGRAPH_DEPLOYMENT};
    use tokio::sync::watch;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_allocation_middleware() {
        let deployment = *ESCROW_SUBGRAPH_DEPLOYMENT;
        let deployment_to_allocation =
            watch::channel(vec![(deployment, Address::ZERO)].into_iter().collect()).1;
        let state = AllocationState {
            deployment_to_allocation,
        };

        let middleware = from_fn_with_state(state, allocation_middleware);

        async fn handle(extensions: Extensions) -> Body {
            let allocation = extensions
                .get::<Allocation>()
                .expect("Should contain allocation");
            assert_eq!(allocation.0, Address::ZERO);
            Body::empty()
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let receipt = create_signed_receipt(Address::ZERO, 1, 1, 1).await;

        // with receipt
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(receipt)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // with deployment
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(deployment)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}