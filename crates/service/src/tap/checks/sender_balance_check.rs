// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use indexer_monitor::EscrowAccounts;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::U256;
use tokio::sync::watch::Receiver;

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

pub struct SenderBalanceCheck {
    escrow_accounts_v1: Receiver<EscrowAccounts>,
    escrow_accounts_v2: Receiver<EscrowAccounts>,
}

impl SenderBalanceCheck {
    pub fn new(
        escrow_accounts_v1: Receiver<EscrowAccounts>,
        escrow_accounts_v2: Receiver<EscrowAccounts>,
    ) -> Self {
        Self {
            escrow_accounts_v1,
            escrow_accounts_v2,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for SenderBalanceCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let escrow_accounts_snapshot_v1 = self.escrow_accounts_v1.borrow();
        let escrow_accounts_snapshot_v2 = self.escrow_accounts_v2.borrow();

        let Sender(receipt_sender) = ctx
            .get::<Sender>()
            .ok_or(CheckError::Failed(anyhow::anyhow!("Could not find sender")))?;

        // get balance for escrow account given receipt type
        let balance_result = match receipt.signed_receipt() {
            TapReceipt::V1(_) => escrow_accounts_snapshot_v1.get_balance_for_sender(receipt_sender),
            TapReceipt::V2(_) => escrow_accounts_snapshot_v2.get_balance_for_sender(receipt_sender),
        };

        // Check that the sender has a non-zero balance -- more advanced accounting is done in
        // `tap-agent`.
        if !balance_result.is_ok_and(|balance| balance > U256::ZERO) {
            return Err(CheckError::Failed(anyhow!(
                "Receipt sender `{}` does not have a sufficient balance",
                receipt_sender,
            )));
        }
        Ok(())
    }
}
