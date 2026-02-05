// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use thegraph_core::{AllocationId, CollectionId, DeploymentId};
use tokio::sync::watch;

use crate::tap::TapReceipt;

/// State to be used by allocation middleware
#[derive(Clone)]
pub struct AllocationState {
    /// watcher that maps deployment ids to allocation ids
    pub deployment_to_allocation: watch::Receiver<HashMap<DeploymentId, AllocationId>>,
}

/// Injects allocation id in extensions
/// - check if allocation id already exists
/// - else, try to fetch allocation id from deployment_id to allocations map
///
/// Requires TapReceipt extension to be added OR deployment id
pub async fn allocation_middleware(
    State(my_state): State<AllocationState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(receipt) = request.extensions().get::<TapReceipt>() {
        if let TapReceipt::V2(r) = receipt {
            let allocation_id = AllocationId::from(CollectionId::from(r.message.collection_id));
            request.extensions_mut().insert(allocation_id);
        }
    } else if let Some(deployment_id) = request.extensions().get::<DeploymentId>() {
        if let Some(allocation_id) = my_state
            .deployment_to_allocation
            .borrow()
            .get(deployment_id)
        {
            request.extensions_mut().insert(*allocation_id);
        }
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use test_assets::{
        create_signed_receipt_v2, ALLOCATION_ID_0, COLLECTION_ID_0, ESCROW_SUBGRAPH_DEPLOYMENT,
    };
    use thegraph_core::{AllocationId, DeploymentId};
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::{allocation_middleware, AllocationState};
    use crate::tap::TapReceipt;

    fn create_state_with_deployment_mapping(
        deployment: DeploymentId,
        allocation: AllocationId,
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
    async fn test_extracts_correct_allocation_id_from_tap_receipt_v2() {
        // Verify the middleware derives the EXACT correct AllocationId from CollectionId.
        // This tests the core conversion: AllocationId::from(CollectionId) truncates to last 20 bytes.
        let expected_allocation = AllocationId::from(COLLECTION_ID_0);
        let state = create_empty_state();
        let middleware = from_fn_with_state(state, allocation_middleware);

        let captured = Arc::new(Mutex::new(None::<AllocationId>));
        let captured_clone = captured.clone();

        let app = Router::new()
            .route(
                "/",
                get(move |extensions: Extensions| {
                    let captured = captured_clone.clone();
                    async move {
                        match extensions.get::<AllocationId>() {
                            Some(id) => {
                                *captured.lock().unwrap() = Some(*id);
                                StatusCode::OK
                            }
                            None => StatusCode::BAD_REQUEST,
                        }
                    }
                }),
            )
            .layer(middleware);

        let receipt = create_signed_receipt_v2()
            .collection_id(COLLECTION_ID_0)
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

        let actual_allocation = captured
            .lock()
            .unwrap()
            .expect("Handler should have captured AllocationId");
        assert_eq!(
            actual_allocation, expected_allocation,
            "V2 receipt CollectionId {COLLECTION_ID_0} should derive AllocationId {expected_allocation}"
        );
    }

    #[tokio::test]
    async fn test_falls_back_to_deployment_mapping_with_correct_value() {
        // Verify deployment fallback returns the EXACT allocation from the mapping
        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;
        let expected_allocation = AllocationId::from(ALLOCATION_ID_0);
        let state = create_state_with_deployment_mapping(deployment, expected_allocation);
        let middleware = from_fn_with_state(state, allocation_middleware);

        let captured = Arc::new(Mutex::new(None::<AllocationId>));
        let captured_clone = captured.clone();

        let app = Router::new()
            .route(
                "/",
                get(move |extensions: Extensions| {
                    let captured = captured_clone.clone();
                    async move {
                        match extensions.get::<AllocationId>() {
                            Some(id) => {
                                *captured.lock().unwrap() = Some(*id);
                                StatusCode::OK
                            }
                            None => StatusCode::BAD_REQUEST,
                        }
                    }
                }),
            )
            .layer(middleware);

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

        let actual_allocation = captured
            .lock()
            .unwrap()
            .expect("Handler should have captured AllocationId");
        assert_eq!(
            actual_allocation, expected_allocation,
            "Deployment fallback should return exact allocation from mapping"
        );
    }

    #[tokio::test]
    async fn test_v2_collection_id_to_allocation_id_conversion_is_deterministic() {
        // Verify the conversion is deterministic: same CollectionId always produces same AllocationId
        let collection_id = COLLECTION_ID_0;
        let allocation_1 = AllocationId::from(collection_id);
        let allocation_2 = AllocationId::from(collection_id);

        assert_eq!(
            allocation_1, allocation_2,
            "CollectionId to AllocationId conversion must be deterministic"
        );

        // Verify it extracts the last 20 bytes (the address portion)
        // CollectionId is a B256 (32 bytes), AllocationId takes the last 20 bytes
        let collection_fixed_bytes = collection_id.as_ref(); // &FixedBytes<32>
        let collection_bytes: &[u8] = collection_fixed_bytes.as_ref(); // &[u8]
        let expected_address_bytes: &[u8] = &collection_bytes[12..];
        assert_eq!(
            allocation_1.into_inner().as_slice(),
            expected_address_bytes,
            "AllocationId should contain the last 20 bytes of CollectionId"
        );
    }

    #[tokio::test]
    async fn test_no_allocation_when_neither_receipt_nor_deployment_present() {
        let state = create_empty_state();
        let middleware = from_fn_with_state(state, allocation_middleware);

        let app = Router::new()
            .route(
                "/",
                get(|extensions: Extensions| async move {
                    if extensions.get::<AllocationId>().is_some() {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                }),
            )
            .layer(middleware);

        let res = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "No AllocationId should be injected when neither receipt nor deployment is present"
        );
    }

    #[tokio::test]
    async fn test_no_allocation_when_deployment_not_in_mapping() {
        let deployment = ESCROW_SUBGRAPH_DEPLOYMENT;
        let state = create_empty_state(); // Empty mapping

        let middleware = from_fn_with_state(state, allocation_middleware);

        let app = Router::new()
            .route(
                "/",
                get(|extensions: Extensions| async move {
                    if extensions.get::<AllocationId>().is_some() {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                }),
            )
            .layer(middleware);

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
            "No AllocationId should be injected when deployment is not in mapping"
        );
    }
}
