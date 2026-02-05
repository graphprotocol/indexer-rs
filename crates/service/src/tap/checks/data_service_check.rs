// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};

use crate::tap::{CheckingReceipt, TapReceipt};

/// Validates that the receipt's `data_service` field matches an
/// allowed SubgraphService address (or one of them).
///
/// - On mismatch, returns a CheckFailure with a descriptive message.
pub struct DataServiceCheck {
    allowed: Vec<Address>,
}

impl DataServiceCheck {
    pub fn new(allowed: Vec<Address>) -> Self {
        Self { allowed }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for DataServiceCheck {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let got = receipt.signed_receipt().as_ref().message.data_service;
        if self.allowed.contains(&got) {
            Ok(())
        } else {
            Err(CheckError::Failed(anyhow::anyhow!(
                "Invalid data_service: {} is not allowed for this indexer",
                got.encode_hex()
            )))
        }
    }
}
