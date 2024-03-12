use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use ethereum_types::U256;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
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
            .recover_signer(&self.domain_separator)?;
        let escrow_accounts =
            self.escrow_accounts
                .value()
                .await
                .map_err(|e| AdapterError::ValidationError {
                    error: format!("Could not get escrow accounts from eventual: {:?}", e),
                })?;

        let sender = escrow_accounts.get_sender_for_signer(&signer)?;

        let balance = escrow_accounts.get_balance_for_sender(&sender)?;

        if balance == U256::from(0) {
            Err(anyhow!(
                "Balance for sender {}, signer {} is not positive",
                sender,
                signer
            ))
        } else {
            Ok(())
        }
    }
}
