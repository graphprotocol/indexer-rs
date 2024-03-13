// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::escrow_accounts::EscrowAccounts;
use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use ethers_core::types::U256;
use eventuals::Eventual;
use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
};
use tracing::error;

pub struct SenderBalanceCheck {
    escrow_accounts: Eventual<EscrowAccounts>,

    domain_separator: Eip712Domain,
}

impl SenderBalanceCheck {
    pub fn new(escrow_accounts: Eventual<EscrowAccounts>, domain_separator: Eip712Domain) -> Self {
        Self {
            escrow_accounts,
            domain_separator,
        }
    }
}

#[async_trait::async_trait]
impl Check for SenderBalanceCheck {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let escrow_accounts_snapshot = self.escrow_accounts.value_immediate().unwrap_or_default();

        let receipt_signer = receipt
            .signed_receipt()
            .recover_signer(&self.domain_separator)
            .inspect_err(|e| {
                error!("Failed to recover receipt signer: {}", e);
            })?;

        // We bail if the receipt signer does not have a corresponding sender in the escrow
        // accounts.
        let receipt_sender = escrow_accounts_snapshot.get_sender_for_signer(&receipt_signer)?;

        // Check that the sender has a non-zero balance -- more advanced accounting is done in
        // `tap-agent`.
        if !escrow_accounts_snapshot
            .get_balance_for_sender(&receipt_sender)
            .map_or(false, |balance| balance > U256::zero())
        {
            return Err(anyhow!(
                "Receipt sender `{}` does not have a sufficient balance",
                receipt_signer,
            ));
        }
        Ok(())
    }
}
