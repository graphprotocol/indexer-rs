// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
use anyhow::anyhow;

pub struct ReceiptMaxValueCheck {
    receipt_max_value: u128,
}

use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use tap_graph::SignedReceipt;

use crate::tap::CheckingReceipt;

impl ReceiptMaxValueCheck {
    pub fn new(receipt_max_value: u128) -> Self {
        Self { receipt_max_value }
    }
}

#[async_trait::async_trait]
impl Check<SignedReceipt> for ReceiptMaxValueCheck {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let receipt_value = receipt.signed_receipt().message.value;

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
    use std::time::{Duration, SystemTime};

    use tap_core::{
        receipt::{checks::Check, Context},
        signed_message::Eip712SignedMessage,
        tap_eip712_domain,
    };
    use tap_graph::Receipt;
    use thegraph_core::alloy::{
        primitives::{address, Address},
        signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    };

    use super::*;
    use crate::tap::{CheckingReceipt, Eip712Domain};

    fn create_signed_receipt_with_custom_value(value: u128) -> CheckingReceipt {
        let index: u32 = 0;
        let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .index(index)
            .unwrap()
            .build()
            .unwrap();

        let eip712_domain_separator: Eip712Domain =
            tap_eip712_domain(1, Address::from([0x11u8; 20]));

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos()
            + Duration::from_secs(33).as_nanos();
        let timestamp_ns = timestamp as u64;

        let value: u128 = value;
        let nonce: u64 = 10;
        let receipt = Eip712SignedMessage::new(
            &eip712_domain_separator,
            Receipt {
                allocation_id: address!("abababababababababababababababababababab"),
                nonce,
                timestamp_ns,
                value,
            },
            &wallet,
        )
        .unwrap();
        CheckingReceipt::new(receipt)
    }

    const RECEIPT_LIMIT: u128 = 10;
    #[tokio::test]
    async fn test_receipt_lower_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT - 1);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_receipt_higher_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT + 1);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_receipt_same_as_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }
}
