// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::Address;

use crate::tap::{CheckingReceipt, TapReceipt};

/// Validates that the V2 receipt's `service_provider` field matches
/// this indexer's address.
///
/// - V1 receipts are ignored by this check (always Ok).
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
        match receipt.signed_receipt() {
            TapReceipt::V2(r) => {
                if self.indexer_address == r.message.service_provider {
                    Ok(())
                } else {
                    Err(CheckError::Failed(anyhow::anyhow!(
                        "Invalid service_provider: receipt is not addressed to this indexer",
                    )))
                }
            }
            TapReceipt::V1(_) => Err(CheckError::Failed(anyhow::anyhow!(
                "Receipt v1 received but Horizon-only mode is enabled"
            ))),
        }
    }
}
