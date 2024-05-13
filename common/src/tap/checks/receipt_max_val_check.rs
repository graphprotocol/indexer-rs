// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
use anyhow::anyhow;

pub struct ReceiptMaxValueCheck {
    receipt_max_value: u128,
}

use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
};

impl ReceiptMaxValueCheck {
    pub fn new(receipt_max_value: u128) -> Self {
        Self { receipt_max_value }
    }
}

#[async_trait::async_trait]
impl Check for ReceiptMaxValueCheck {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let receipt_value = receipt.signed_receipt().message.value;

        if receipt_value < self.receipt_max_value {
            Ok(())
        } else {
            Err(anyhow!(
                "Receipt value `{}` is higher than the limit set by the user",
                receipt_value
            ))
        }
    }
}
#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;
    use std::time::SystemTime;

    use alloy_primitives::Address;
    use alloy_sol_types::eip712_domain;
    use alloy_sol_types::Eip712Domain;

    use ethers::signers::coins_bip39::English;
    use ethers::signers::{LocalWallet, MnemonicBuilder};

    use super::*;
    use tap_core::{
        receipt::{checks::Check, Checking, Receipt, ReceiptWithState},
        signed_message::EIP712SignedMessage,
    };

    fn create_signed_receipt_with_custom_value(value: u128) -> ReceiptWithState<Checking> {
        let index: u32 = 0;
        let wallet: LocalWallet = MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .index(index)
            .unwrap()
            .build()
            .unwrap();
        let eip712_domain_separator: Eip712Domain = eip712_domain! {
            name: "TAP",
            version: "1",
            chain_id: 1,
            verifying_contract: Address:: from([0x11u8; 20]),
        };

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos()
            + Duration::from_secs(33).as_nanos();
        let timestamp_ns = timestamp as u64;

        let value: u128 = value;
        let nonce: u64 = 10;
        let receipt = EIP712SignedMessage::new(
            &eip712_domain_separator,
            Receipt {
                allocation_id: Address::from_str("0xabababababababababababababababababababab")
                    .unwrap(),
                nonce,
                timestamp_ns,
                value,
            },
            &wallet,
        )
        .unwrap();
        ReceiptWithState::<Checking>::new(receipt)
    }

    const RECEIPT_LIMIT: u128 = 10;
    #[tokio::test]
    async fn test_receipt_lower_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT - 1);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check.check(&signed_receipt).await.is_ok());
    }

    #[tokio::test]
    async fn test_receipt_higher_than_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT + 1);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check.check(&signed_receipt).await.is_err());
    }

    #[tokio::test]
    async fn test_receipt_same_as_limit() {
        let signed_receipt = create_signed_receipt_with_custom_value(RECEIPT_LIMIT);
        let timestamp_check = ReceiptMaxValueCheck::new(RECEIPT_LIMIT);
        assert!(timestamp_check.check(&signed_receipt).await.is_err());
    }
}
