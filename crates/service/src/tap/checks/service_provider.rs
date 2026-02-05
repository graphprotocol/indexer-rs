// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::Address;

use crate::tap::{CheckingReceipt, TapReceipt};

/// Validates that the receipt's `service_provider` field matches
/// this indexer's address.
///
/// - On mismatch, returns a CheckFailure with a descriptive message.
pub struct ServiceProviderCheck {
    indexer_address: Address,
}

impl ServiceProviderCheck {
    pub fn new(indexer_address: Address) -> Self {
        Self { indexer_address }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for ServiceProviderCheck {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let receipt = receipt.signed_receipt().get_v2_receipt();
        if self.indexer_address == receipt.message.service_provider {
            Ok(())
        } else {
            Err(CheckError::Failed(anyhow::anyhow!(
                "Invalid service_provider: receipt is not addressed to this indexer",
            )))
        }
    }
}
