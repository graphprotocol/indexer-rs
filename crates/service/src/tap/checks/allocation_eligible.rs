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
        let allocation_id = AllocationId::from(CollectionId::from(
            receipt
                .signed_receipt()
                .get_v2_receipt()
                .message
                .collection_id,
        ));
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
