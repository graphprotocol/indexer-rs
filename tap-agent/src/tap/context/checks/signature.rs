// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{dyn_abi::Eip712Domain, primitives::U256};
use anyhow::anyhow;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    ReceiptWithState,
};

use crate::tap::context::error::AdapterError;

pub struct Signature {
    domain_separator: Eip712Domain,
    escrow_accounts: Eventual<EscrowAccounts>,
}

impl Signature {
    pub fn new(domain_separator: Eip712Domain, escrow_accounts: Eventual<EscrowAccounts>) -> Self {
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
        let escrow_accounts = self
            .escrow_accounts
            .value()
            .await
            .map_err(|e| AdapterError::ValidationError {
                error: format!("Could not get escrow accounts from eventual: {:?}", e),
            })
            .map_err(|e| CheckError::Retryable(e.into()))?;

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
