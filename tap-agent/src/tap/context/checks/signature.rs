// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{dyn_abi::Eip712Domain, primitives::U256};
use anyhow::anyhow;
use indexer_common::escrow_accounts::EscrowAccounts;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    ReceiptWithState,
};
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
impl Check for Signature {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
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
