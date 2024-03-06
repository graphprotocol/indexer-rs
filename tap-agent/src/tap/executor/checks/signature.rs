use alloy_sol_types::Eip712Domain;
use ethereum_types::U256;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use serde::{Deserialize, Serialize};
use tap_core::{
    checks::{Check, CheckError, CheckResult},
    tap_receipt::{Checking, ReceiptWithState},
};

use crate::tap::executor::error::AdapterError;

#[derive(Serialize, Deserialize)]
pub struct Signature {
    domain_separator: Eip712Domain,
    #[serde(skip)]
    #[serde(default = "default_eventual")]
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

fn default_eventual() -> Eventual<EscrowAccounts> {
    let (_, evt) = Eventual::new();
    evt
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SenderId").finish()
    }
}

#[async_trait::async_trait]
#[typetag::serde]
impl Check for Signature {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult<()> {
        let signer = receipt
            .signed_receipt()
            .recover_signer(&self.domain_separator)
            .map_err(|e| CheckError(e.to_string()))?;
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
            Err(CheckError(format!(
                "Balance for sender {}, signer {} is not positive",
                sender, signer
            )))
        } else {
            Ok(())
        }
    }
}
