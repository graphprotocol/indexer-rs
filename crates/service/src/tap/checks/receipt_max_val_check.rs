// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
use anyhow::anyhow;

pub struct ReceiptMaxValueCheck {
    receipt_max_value: u128,
}

use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    WithValueAndTimestamp,
};

use crate::tap::{CheckingReceipt, TapReceipt};

impl ReceiptMaxValueCheck {
    pub fn new(receipt_max_value: u128) -> Self {
        Self { receipt_max_value }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for ReceiptMaxValueCheck {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let receipt_value = receipt.signed_receipt().value();

        if receipt_value < self.receipt_max_value {
            Ok(())
        } else {
            Err(CheckError::Failed(anyhow!(
                "Receipt value `{}` is higher than the limit set by the user",
                receipt_value
            )))
        }
    }
}
#[cfg(test)]
mod tests {
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::create_signed_receipt_v2;

    use super::*;
    use crate::tap::CheckingReceipt;

    async fn create_signed_receipt_with_custom_value(value: u128) -> CheckingReceipt {
        let receipt = create_signed_receipt_v2().value(value).call().await;
        CheckingReceipt::new(TapReceipt::V2(receipt))
    }

    const RECEIPT_LIMIT: u128 = 10;
    #[tokio::test]
    async fn test_receipt_lower_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT - 1).await;
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_receipt_higher_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT + 1).await;
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_receipt_same_as_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT).await;
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }
}
