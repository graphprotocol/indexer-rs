// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::anyhow;

use indexer_common::allocations::Allocation;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    ReceiptWithState,
};
use tokio::sync::watch::Receiver;

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
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &ReceiptWithState<Checking>,
    ) -> CheckResult {
        let allocation_id = receipt.signed_receipt().message.allocation_id;
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
