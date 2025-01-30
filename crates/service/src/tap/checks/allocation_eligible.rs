// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use anyhow::anyhow;
use indexer_allocation::Allocation;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::Address;
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
        let allocation_id = receipt.signed_receipt().allocation_id();
        if !self
            .indexer_allocations
            .borrow()
            .contains_key(&allocation_id)
        {
            return Err(CheckError::Failed(anyhow!(
                "Receipt allocation ID `{}` is not eligible for this indexer",
                allocation_id
            )));
        }
        Ok(())
    }
}
