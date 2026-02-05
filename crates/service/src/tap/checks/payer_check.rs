// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use tap_core::receipt::checks::{Check, CheckError, CheckResult};

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

/// Validates that the receipt's `payer` field matches the on-chain
/// recovered sender (from signer → payer escrow mapping).
///
/// - On mismatch, returns a CheckFailure with a descriptive message.
///
/// This prevents attackers from submitting receipts with mismatched payer
/// fields that would be stored but never aggregated by tap-agent.
pub struct PayerCheck;

impl PayerCheck {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PayerCheck {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for PayerCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        // Get the recovered sender from context (injected by sender middleware)
        let Sender(recovered_sender) = ctx.get::<Sender>().ok_or_else(|| {
            CheckError::Failed(anyhow::anyhow!(
                "Could not find recovered sender in context"
            ))
        })?;

        let receipt = receipt.signed_receipt().get_v2_receipt();
        // Compare claimed payer against on-chain recovered sender
        if receipt.message.payer == *recovered_sender {
            Ok(())
        } else {
            Err(CheckError::Failed(anyhow::anyhow!(
                "Invalid payer: receipt claims payer {} but signer is authorized for {}",
                receipt.message.payer,
                recovered_sender
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use indexer_monitor::EscrowAccounts;
    use tap_core::{
        receipt::Context, signed_message::Eip712SignedMessage, tap_eip712_domain, TapVersion,
    };
    use test_assets::{TAP_SENDER, TAP_SIGNER};
    use thegraph_core::alloy::{
        primitives::{Address, FixedBytes, U256},
        signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    };
    use tokio::sync::watch;

    use super::*;

    fn create_escrow_accounts() -> watch::Receiver<EscrowAccounts> {
        watch::channel(EscrowAccounts::new(
            HashMap::from([(TAP_SENDER.1, U256::from(1000))]),
            HashMap::from([(TAP_SENDER.1, vec![TAP_SIGNER.1])]),
        ))
        .1
    }

    fn create_wallet() -> PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .index(0)
            .unwrap()
            .build()
            .unwrap()
    }

    fn create_v2_receipt(payer: Address) -> CheckingReceipt {
        let wallet = create_wallet();
        let eip712_domain = tap_eip712_domain(1, Address::from([0x11u8; 20]), TapVersion::V2);

        let receipt = tap_graph::v2::Receipt {
            payer,
            data_service: Address::ZERO,
            service_provider: Address::ZERO,
            collection_id: FixedBytes::ZERO,
            timestamp_ns: 1000,
            nonce: 1,
            value: 100,
        };

        let signed = Eip712SignedMessage::new(&eip712_domain, receipt, &wallet).unwrap();
        CheckingReceipt::new(TapReceipt::V2(signed))
    }

    #[tokio::test]
    async fn test_payer_check_passes_when_payer_matches() {
        let _escrow = create_escrow_accounts();
        let check = PayerCheck::new();

        // Create context with recovered sender
        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        // Create receipt with matching payer
        let receipt = create_v2_receipt(TAP_SENDER.1);

        let result = check.check(&ctx, &receipt).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_payer_check_fails_when_payer_mismatches() {
        let _escrow = create_escrow_accounts();
        let check = PayerCheck::new();

        // Create context with recovered sender
        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        // Create receipt with different payer (attacker scenario)
        let fake_payer = Address::repeat_byte(0x42);
        let receipt = create_v2_receipt(fake_payer);

        let result = check.check(&ctx, &receipt).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("Invalid payer"));
    }

    #[tokio::test]
    async fn test_payer_check_fails_when_no_sender_in_context() {
        let check = PayerCheck::new();

        // Empty context (no Sender)
        let ctx = Context::new();
        let receipt = create_v2_receipt(TAP_SENDER.1);

        let result = check.check(&ctx, &receipt).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("Could not find recovered sender"));
    }
}
