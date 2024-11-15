// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::{
    escrow_accounts::EscrowAccounts,
    tap::checks::{
        deny_list_check::DenyListCheck, receipt_max_val_check::ReceiptMaxValueCheck,
        sender_balance_check::SenderBalanceCheck, timestamp_check::TimestampCheck,
    },
};
use alloy::dyn_abi::Eip712Domain;
use receipt_store::{DatabaseReceipt, InnerContext};
use sqlx::PgPool;
use std::{fmt::Debug, sync::Arc, time::Duration};
use tap_core::receipt::checks::ReceiptCheck;
use tokio::sync::{
    mpsc::{self, Sender},
    watch::Receiver,
};
use tokio_util::sync::CancellationToken;
use tracing::error;

mod checks;
mod receipt_store;

#[derive(Clone)]
pub struct IndexerTapContext {
    domain_separator: Arc<Eip712Domain>,
    receipt_producer: Sender<DatabaseReceipt>,
    cancelation_token: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error(transparent)]
    AnyhowError(#[from] anyhow::Error),
}

impl IndexerTapContext {
    pub async fn get_checks(
        pgpool: PgPool,
        escrow_accounts: Receiver<EscrowAccounts>,
        domain_separator: Eip712Domain,
        timestamp_error_tolerance: Duration,
        receipt_max_value: u128,
    ) -> Vec<ReceiptCheck> {
        vec![
            Arc::new(SenderBalanceCheck::new(
                escrow_accounts.clone(),
                domain_separator.clone(),
            )),
            Arc::new(TimestampCheck::new(timestamp_error_tolerance)),
            Arc::new(DenyListCheck::new(pgpool.clone(), escrow_accounts, domain_separator).await),
            Arc::new(ReceiptMaxValueCheck::new(receipt_max_value)),
        ]
    }

    pub async fn new(pgpool: PgPool, domain_separator: Eip712Domain) -> Self {
        const MAX_RECEIPT_QUEUE_SIZE: usize = 1000;
        let (tx, rx) = mpsc::channel(MAX_RECEIPT_QUEUE_SIZE);
        let cancelation_token = CancellationToken::new();
        let inner = InnerContext { pgpool };
        Self::spawn_store_receipt_task(inner, rx, cancelation_token.clone());

        Self {
            cancelation_token,
            receipt_producer: tx,
            domain_separator: Arc::new(domain_separator),
        }
    }
}

impl Drop for IndexerTapContext {
    fn drop(&mut self) {
        self.cancelation_token.cancel();
    }
}
