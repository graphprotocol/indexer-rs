// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use indexer_monitor::EscrowAccounts;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::{primitives::U256, sol_types::Eip712Domain};
use tokio::sync::watch::Receiver;

use crate::tap::{CheckingReceipt, TapReceipt};

/// Signature check
///
/// Verifies if the signatures are signed correctly by the list of provided signers.
/// This is an important step since [tap_core] doesn't verify signatures by default and RavRequests
/// may fail if any of those are wrong.
pub struct Signature {
    domain_separator: Eip712Domain,
    escrow_accounts: Receiver<EscrowAccounts>,
}

impl Signature {
    /// Creates a new signature check
    pub fn new(domain_separator: Eip712Domain, escrow_accounts: Receiver<EscrowAccounts>) -> Self {
        Self {
            domain_separator,
            escrow_accounts,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for Signature {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
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
