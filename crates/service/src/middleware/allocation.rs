// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use thegraph_core::{alloy::primitives::Address, CollectionId, DeploymentId};

use crate::tap::TapReceipt;
use tokio::sync::watch;

/// The current query Allocation ID address
// TODO: Use thegraph-core::AllocationId instead
#[derive(Clone)]
pub struct Allocation(pub Address);

impl From<Allocation> for String {
    fn from(value: Allocation) -> Self {
        value.0.to_string()
    }
}

/// State to be used by allocation middleware
#[derive(Clone)]
pub struct AllocationState {
    /// watcher that maps deployment ids to allocation ids
    pub deployment_to_allocation: watch::Receiver<HashMap<DeploymentId, Address>>,
}

/// Injects allocation id in extensions
/// - check if allocation id already exists
/// - else, try to fetch allocation id from deployment_id to allocations map
///
/// Requires signed receipt Extension to be added OR deployment id
pub async fn allocation_middleware(
    State(my_state): State<AllocationState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(receipt) = request.extensions().get::<TapReceipt>() {
        let allocation = match receipt {
            TapReceipt::V1(r) => r.message.allocation_id,
            TapReceipt::V2(r) => CollectionId::from(r.message.collection_id).as_address(),
        };
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
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use test_assets::{create_signed_receipt, SignedReceiptRequest, ESCROW_SUBGRAPH_DEPLOYMENT};

    use crate::tap::TapReceipt;
    use thegraph_core::alloy::primitives::Address;
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::{allocation_middleware, Allocation, AllocationState};

    #[tokio::test]
    async fn test_allocation_middleware() {
        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;
        let (_, deployment_to_allocation) =
            watch::channel(vec![(deployment, Address::ZERO)].into_iter().collect());
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

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        let tap_receipt = TapReceipt::V1(receipt);

        // with receipt
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(tap_receipt)
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
