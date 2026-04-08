// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use anyhow::anyhow;
use indexer_allocation::Allocation;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{alloy::primitives::Address, AllocationId, CollectionId};
use tokio::sync::watch::Receiver;

use crate::tap::{CheckingReceipt, TapReceipt};

pub struct AllocationEligible {
    indexer_allocations: Receiver<HashMap<Address, Allocation>>,
}

impl AllocationEligible {
    pub fn new(indexer_allocations: Receiver<HashMap<Address, Allocation>>) -> Self {
        Self {
            indexer_allocations,
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
        if !self
            .indexer_allocations
            .borrow()
            .contains_key(&allocation_address)
        {
            return Err(CheckError::Failed(anyhow!(
                "Receipt allocation ID `{}` is not eligible for this indexer",
                allocation_id
            )));
        }
        Ok(())
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
