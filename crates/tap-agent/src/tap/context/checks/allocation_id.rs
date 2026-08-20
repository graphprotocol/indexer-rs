// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{alloy::primitives::Address, CollectionId};

use crate::tap::{CheckingReceipt, TapReceipt};

/// AllocationId check
///
/// Verifies that a receipt is addressed to the allocation this actor serves.
///
/// Replay protection is deliberately not done here. `tap_core` only collects receipts newer than
/// the last RAV, the sender's aggregator refuses to sign a RAV containing a receipt at or below
/// the previous RAV's timestamp, and `GraphTallyCollector` pays out only the delta over what it
/// has already collected for the collection.
pub struct AllocationId {
    allocation_id: Address,
    collection_id: CollectionId,
}

impl AllocationId {
    /// Creates a new allocation id check
    pub fn new(allocation_id: Address, collection_id: CollectionId) -> Self {
        Self {
            allocation_id,
            collection_id,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationId {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let collection_id = receipt.signed_receipt().collection_id();
        if self.collection_id != collection_id {
            return Err(CheckError::Failed(anyhow!(
                "Receipt collection_id different from expected: collection_id: {:?}, expected_collection_id: {}",
                collection_id,
                self.collection_id
            )));
        }
        let allocation_id = self.collection_id.as_address();

        tracing::debug!(
            allocation_id = %allocation_id,
            expected_allocation_id = %self.allocation_id,
            "Checking allocation_id",
        );
        if allocation_id != self.allocation_id {
            return Err(CheckError::Failed(anyhow!("Receipt allocation_id different from expected: allocation_id: {:?}, expected_allocation_id: {}", allocation_id, self.allocation_id)));
        };

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{ALLOCATION_ID_0, ALLOCATION_ID_1, COLLECTION_ID_0, TAP_SIGNER as SIGNER};
    use thegraph_core::CollectionId;

    use crate::test::create_received_receipt_v2;

    #[tokio::test]
    async fn test_receipt_for_this_allocation_is_accepted() {
        let check = super::AllocationId::new(ALLOCATION_ID_0, COLLECTION_ID_0);
        let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_ok(),
            "expected a receipt for this allocation to be accepted, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_receipt_for_another_allocation_is_rejected() {
        let check = super::AllocationId::new(ALLOCATION_ID_0, COLLECTION_ID_0);
        let receipt = create_received_receipt_v2(&ALLOCATION_ID_1, &SIGNER.0, 1, 1, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_err(),
            "expected a receipt carrying another allocation's collection_id to be rejected"
        );
    }

    #[tokio::test]
    async fn test_receipt_is_accepted_regardless_of_timestamp() {
        // Anti-replay is enforced by tap_core's min_timestamp, the sender's aggregator and the
        // collector contract -- never by this check. An old receipt is not this check's problem.
        let check = super::AllocationId::new(ALLOCATION_ID_0, COLLECTION_ID_0);
        let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, 0, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_ok(),
            "expected timestamp to be irrelevant to the allocation id check, got {result:?}"
        );
    }

    #[test]
    fn test_collection_id_0_matches_allocation_id_0() {
        // The two tests above only mean anything if these agree.
        assert_eq!(COLLECTION_ID_0, CollectionId::from(ALLOCATION_ID_0));
    }
}
