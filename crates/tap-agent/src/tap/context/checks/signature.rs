// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use indexer_monitor::EscrowAccounts;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    ReceiptWithState, SignedReceipt,
};
use thegraph_core::alloy::{primitives::U256, sol_types::Eip712Domain};
use tokio::sync::watch::Receiver;

pub struct Signature {
    domain_separator: Eip712Domain,
    escrow_accounts: Receiver<EscrowAccounts>,
}

impl Signature {
    pub fn new(domain_separator: Eip712Domain, escrow_accounts: Receiver<EscrowAccounts>) -> Self {
        Self {
            domain_separator,
            escrow_accounts,
        }
    }
}

#[async_trait::async_trait]
impl Check<SignedReceipt> for Signature {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &ReceiptWithState<Checking, SignedReceipt>,
    ) -> CheckResult {
        let signer = receipt
            .signed_receipt()
            .recover_signer(&self.domain_separator)
            .map_err(|e| CheckError::Failed(e.into()))?;
        let escrow_accounts = self.escrow_accounts.borrow();

        let sender = escrow_accounts
            .get_sender_for_signer(&signer)
            .map_err(|e| CheckError::Failed(e.into()))?;

        let balance = escrow_accounts
            .get_balance_for_sender(&sender)
            .map_err(|e| CheckError::Failed(e.into()))?;

        if balance == U256::ZERO {
            Err(CheckError::Failed(anyhow!(
                "Balance for sender {}, signer {} is not positive",
                sender,
                signer
            )))
        } else {
            Ok(())
        }
    }
}
