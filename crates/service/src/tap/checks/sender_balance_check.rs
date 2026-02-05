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
    escrow_accounts_v2: Option<Receiver<EscrowAccounts>>,
}

impl SenderBalanceCheck {
    pub fn new(escrow_accounts_v2: Option<Receiver<EscrowAccounts>>) -> Self {
        Self { escrow_accounts_v2 }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for SenderBalanceCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        _receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(receipt_sender) = ctx
            .get::<Sender>()
            .ok_or(CheckError::Failed(anyhow::anyhow!("Could not find sender")))?;

        // get balance for escrow account given receipt type
        let balance_result = if let Some(ref escrow_accounts_v2) = self.escrow_accounts_v2 {
            let escrow_accounts_snapshot_v2 = escrow_accounts_v2.borrow();
            escrow_accounts_snapshot_v2.get_balance_for_sender(receipt_sender)
        } else {
            return Err(CheckError::Failed(anyhow!(
                "Receipt v2 received but no escrow accounts v2 watcher is available"
            )));
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
