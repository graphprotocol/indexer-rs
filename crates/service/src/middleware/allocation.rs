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
    use test_assets::{create_signed_receipt_v2, ALLOCATION_ID_0, COLLECTION_ID_0, ESCROW_SUBGRAPH_DEPLOYMENT};

    use crate::tap::TapReceipt;
    use thegraph_core::{alloy::primitives::Address, CollectionId, DeploymentId};
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::{allocation_middleware, Allocation, AllocationState};

    fn create_state_with_deployment_mapping(
        deployment: DeploymentId,
        allocation: Address,
    ) -> AllocationState {
        let (_, deployment_to_allocation) =
            watch::channel(vec![(deployment, allocation)].into_iter().collect());
        AllocationState {
            deployment_to_allocation,
        }
    }

    fn create_empty_state() -> AllocationState {
        let (_, deployment_to_allocation) = watch::channel(std::collections::HashMap::new());
        AllocationState {
            deployment_to_allocation,
        }
    }

    #[tokio::test]
    async fn test_extracts_allocation_id_from_tap_receipt_v2() {
        let collection_id = COLLECTION_ID_0;
        // V2 receipts use collection_id which maps to an address via CollectionId::as_address()
        let expected_allocation = CollectionId::from(collection_id).as_address();
        let state = create_empty_state();
        let middleware = from_fn_with_state(state, allocation_middleware);

        async fn handle(extensions: Extensions) -> (StatusCode, Body) {
            match extensions.get::<Allocation>() {
                Some(allocation) => (StatusCode::OK, Body::from(allocation.0.to_string())),
                None => (StatusCode::BAD_REQUEST, Body::empty()),
            }
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let receipt = create_signed_receipt_v2()
            .collection_id(collection_id)
            .call()
            .await;
        let tap_receipt = TapReceipt::V2(receipt);

        let res = app
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

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let allocation_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(
            allocation_str,
            expected_allocation.to_string(),
            "V2 receipt collection_id should be converted to address correctly"
        );
    }

    #[tokio::test]
    async fn test_falls_back_to_deployment_id_when_no_receipt() {
        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;
        let expected_allocation = ALLOCATION_ID_0;
        let state = create_state_with_deployment_mapping(deployment, expected_allocation);
        let middleware = from_fn_with_state(state, allocation_middleware);

        async fn handle(extensions: Extensions) -> (StatusCode, Body) {
            match extensions.get::<Allocation>() {
                Some(allocation) => (StatusCode::OK, Body::from(allocation.0.to_string())),
                None => (StatusCode::BAD_REQUEST, Body::empty()),
            }
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        // Request without TapReceipt, but with DeploymentId
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

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let allocation_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(
            allocation_str,
            expected_allocation.to_string(),
            "Fallback to deployment_id mapping should work"
        );
    }

    #[tokio::test]
    async fn test_handles_request_without_receipt_or_deployment_id() {
        let state = create_empty_state();
        let middleware = from_fn_with_state(state, allocation_middleware);

        async fn handle(extensions: Extensions) -> StatusCode {
            // Should NOT have allocation when neither receipt nor deployment is present
            if extensions.get::<Allocation>().is_some() {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::OK
            }
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        // Request without TapReceipt or DeploymentId
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "Middleware should handle requests without receipt or deployment gracefully"
        );
    }

    #[tokio::test]
    async fn test_handles_deployment_id_not_in_mapping() {
        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;
        // Create empty state with no deployment mappings
        let state = create_empty_state();
        let middleware = from_fn_with_state(state, allocation_middleware);

        async fn handle(extensions: Extensions) -> StatusCode {
            // Should NOT have allocation when deployment is not in the mapping
            if extensions.get::<Allocation>().is_some() {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::OK
            }
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        // Request with DeploymentId that's not in the mapping
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

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "Middleware should handle unknown deployment IDs gracefully"
        );
    }
}
