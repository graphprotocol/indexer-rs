// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use indexer_allocation::Allocation;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{alloy::primitives::Address, AllocationId, CollectionId};
use tokio::sync::{watch::Receiver, Mutex};

use crate::tap::{CheckingReceipt, TapReceipt};

/// TAP receipt check that validates allocation eligibility.
///
/// This check verifies that the allocation ID in a receipt corresponds to an
/// active or recently-active allocation for this indexer. A grace period is
/// applied to prevent rejecting receipts for allocations that have transiently
/// disappeared from the watch channel due to network subgraph polling gaps.
pub struct AllocationEligible {
    indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    recently_seen: Arc<Mutex<HashMap<Address, Instant>>>,
    grace_period: Duration,
}

impl AllocationEligible {
    /// Creates a new `AllocationEligible` check with the default grace period (3600 seconds).
    pub fn new(indexer_allocations: Receiver<HashMap<Address, Allocation>>) -> Self {
        // Backward-compatible default: matches recently_closed_allocation_buffer_secs default
        Self::with_grace_period(indexer_allocations, Duration::from_secs(3600))
    }

    /// Creates a new `AllocationEligible` check with a custom grace period.
    ///
    /// The grace period determines how long an allocation remains eligible after
    /// disappearing from the active allocations set. This bridges the gap between
    /// the gateway's allocation discovery and the indexer's network subgraph polling.
    pub fn with_grace_period(
        indexer_allocations: Receiver<HashMap<Address, Allocation>>,
        grace_period: Duration,
    ) -> Self {
        Self {
            indexer_allocations,
            recently_seen: Arc::new(Mutex::new(HashMap::new())),
            grace_period,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationEligible {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        // This is the sole enforcement point for canonical collection_id encoding.
        // Downstream conversions (AllocationId::from, CollectionId::as_address) silently
        // discard the first 12 bytes, so without this check a non-zero prefix would pass
        // validation but produce a storage/aggregation mismatch (TRST-H-2).
        let collection_id_bytes = receipt.signed_receipt().as_ref().message.collection_id;
        if collection_id_bytes[..12] != [0u8; 12] {
            return Err(CheckError::Failed(anyhow!(
                "Invalid collection_id: first 12 bytes must be zero, got non-zero prefix"
            )));
        }

        let allocation_id = AllocationId::from(CollectionId::from(collection_id_bytes));
        let allocation_address = allocation_id.into_inner();

        let in_active_set = self
            .indexer_allocations
            .borrow()
            .contains_key(&allocation_address);

        if in_active_set {
            // Fast path: active. Refresh last-seen timestamp.
            let mut recently_seen = self.recently_seen.lock().await;
            recently_seen.insert(allocation_address, Instant::now());
            return Ok(());
        }

        // Slow path: not in active set. Check local grace period cache.
        let mut recently_seen = self.recently_seen.lock().await;

        // Opportunistic eviction of stale entries to keep the map bounded.
        recently_seen.retain(|_, last_seen| last_seen.elapsed() < self.grace_period * 2);

        if let Some(last_seen) = recently_seen.get(&allocation_address) {
            if last_seen.elapsed() < self.grace_period {
                tracing::debug!(
                    allocation_id = %allocation_id,
                    elapsed_secs = last_seen.elapsed().as_secs(),
                    grace_period_secs = self.grace_period.as_secs(),
                    "Accepting receipt for recently-seen allocation (within grace period)"
                );
                return Ok(());
            }
        }

        Err(CheckError::Failed(anyhow!(
            "Receipt allocation ID `{}` is not eligible for this indexer",
            allocation_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use indexer_allocation::Allocation;
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{create_signed_receipt_v2, INDEXER_ALLOCATIONS};
    use thegraph_core::{alloy::primitives::Address, CollectionId};
    use tokio::sync::watch;

    use super::*;
    use crate::tap::TapReceipt;

    async fn create_test_receipt(allocation_id: Address) -> CheckingReceipt {
        let receipt = create_signed_receipt_v2()
            .collection_id(CollectionId::from(allocation_id))
            .call()
            .await;
        CheckingReceipt::new(TapReceipt(receipt))
    }

    #[tokio::test]
    async fn test_active_allocation_accepted() {
        let (tx, rx) = watch::channel((*INDEXER_ALLOCATIONS).clone());
        let check = AllocationEligible::new(rx);

        let allocation_id = *INDEXER_ALLOCATIONS.keys().next().unwrap();
        let receipt = create_test_receipt(allocation_id).await;

        let result = check.check(&Context::default(), &receipt).await;
        assert!(result.is_ok(), "Active allocation should be accepted");

        drop(tx);
    }

    #[tokio::test]
    async fn test_unknown_allocation_rejected() {
        let (tx, rx) = watch::channel((*INDEXER_ALLOCATIONS).clone());
        let check = AllocationEligible::new(rx);

        // Use an allocation ID that's not in the active set
        let unknown_allocation = Address::repeat_byte(0x42);
        let receipt = create_test_receipt(unknown_allocation).await;

        let result = check.check(&Context::default(), &receipt).await;
        assert!(result.is_err(), "Unknown allocation should be rejected");

        drop(tx);
    }

    #[tokio::test]
    async fn test_recently_seen_allocation_accepted_within_grace_period() {
        let allocations: HashMap<Address, Allocation> = (*INDEXER_ALLOCATIONS).clone();
        let allocation_id = *allocations.keys().next().unwrap();

        let (tx, rx) = watch::channel(allocations.clone());
        let check = AllocationEligible::with_grace_period(rx, Duration::from_secs(3600));

        // First, verify the allocation while it's active (this populates recently_seen)
        let receipt = create_test_receipt(allocation_id).await;
        let result = check.check(&Context::default(), &receipt).await;
        assert!(result.is_ok(), "Active allocation should be accepted");

        // Remove the allocation from the active set
        tx.send(HashMap::new()).unwrap();

        // The allocation should still be accepted within the grace period
        let result = check.check(&Context::default(), &receipt).await;
        assert!(
            result.is_ok(),
            "Recently-seen allocation should be accepted within grace period"
        );
    }

    #[tokio::test]
    async fn test_allocation_rejected_after_grace_period() {
        let allocations: HashMap<Address, Allocation> = (*INDEXER_ALLOCATIONS).clone();
        let allocation_id = *allocations.keys().next().unwrap();

        let (tx, rx) = watch::channel(allocations.clone());
        // Use a very short grace period for testing
        let check = AllocationEligible::with_grace_period(rx, Duration::from_millis(10));

        // Verify the allocation while active (populates recently_seen)
        let receipt = create_test_receipt(allocation_id).await;
        let result = check.check(&Context::default(), &receipt).await;
        assert!(result.is_ok());

        // Remove from active set
        tx.send(HashMap::new()).unwrap();

        // Wait for grace period to expire
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should now be rejected
        let result = check.check(&Context::default(), &receipt).await;
        assert!(
            result.is_err(),
            "Allocation should be rejected after grace period expires"
        );
    }
}

#[cfg(test)]
mod tests {
    use tap_core::receipt::{checks::Check, state::Checking, Context, ReceiptWithState};
    use test_assets::{create_signed_receipt_v2, COLLECTION_ID_0, INDEXER_ALLOCATIONS};
    use thegraph_core::collection_id;
    use tokio::sync::watch;

    use super::*;

    fn make_check() -> AllocationEligible {
        let allocations = watch::channel(INDEXER_ALLOCATIONS.clone()).1;
        AllocationEligible::new(allocations)
    }

    fn to_checking(receipt: TapReceipt) -> CheckingReceipt {
        ReceiptWithState::<Checking, _>::new(receipt)
    }

    #[tokio::test]
    async fn test_valid_zero_padded_collection_id_passes() {
        let check = make_check();
        let receipt = create_signed_receipt_v2()
            .collection_id(COLLECTION_ID_0)
            .call()
            .await;
        let receipt = to_checking(TapReceipt(receipt));
        let result = check.check(&Context::new(), &receipt).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_non_zero_prefix_collection_id_rejected() {
        let check = make_check();
        // Same trailing 20 bytes as ALLOCATION_ID_0 but with non-zero prefix
        let bad_collection_id =
            collection_id!("deadbeef0000000000000000fa44c72b753a66591f241c7dc04e8178c30e13af");
        let receipt = create_signed_receipt_v2()
            .collection_id(bad_collection_id)
            .call()
            .await;
        let receipt = to_checking(TapReceipt(receipt));
        let result = check.check(&Context::new(), &receipt).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("first 12 bytes must be zero"));
    }

    #[tokio::test]
    async fn test_unknown_allocation_rejected() {
        let check = make_check();
        let unknown =
            collection_id!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let receipt = create_signed_receipt_v2()
            .collection_id(unknown)
            .call()
            .await;
        let receipt = to_checking(TapReceipt(receipt));
        let result = check.check(&Context::new(), &receipt).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not eligible for this indexer"));
    }
}
