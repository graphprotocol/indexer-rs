// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::anyhow;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    ReceiptWithState,
};
use tokio::sync::watch::Receiver;

use crate::prelude::Allocation;
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
impl Check for AllocationEligible {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let allocation_id = receipt.signed_receipt().message.allocation_id;
        let indexer_allocations = self.indexer_allocations.borrow().clone();
        if !indexer_allocations.contains_key(&allocation_id) {
            return Err(CheckError::Failed(anyhow!(
                "Receipt allocation ID `{}` is not eligible for this indexer",
                allocation_id
            )));
        }
        Ok(())
    }
}
