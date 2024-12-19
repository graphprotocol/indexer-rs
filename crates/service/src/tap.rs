// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, fmt::Debug, sync::Arc, time::Duration};

use indexer_allocation::Allocation;
use indexer_monitor::EscrowAccounts;
use receipt_store::{DatabaseReceipt, InnerContext};
use sqlx::PgPool;
use tap_core::receipt::checks::ReceiptCheck;
use thegraph_core::alloy::{primitives::Address, sol_types::Eip712Domain};
use tokio::sync::{
    mpsc::{self, Sender},
    watch::Receiver,
};
use tokio_util::sync::CancellationToken;

use crate::tap::checks::{
    allocation_eligible::AllocationEligible, deny_list_check::DenyListCheck,
    receipt_max_val_check::ReceiptMaxValueCheck, sender_balance_check::SenderBalanceCheck,
    timestamp_check::TimestampCheck, value_check::MinimumValue,
};

mod checks;
mod receipt_store;

pub use checks::value_check::AgoraQuery;

const GRACE_PERIOD: u64 = 60;

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
        indexer_allocations: Receiver<HashMap<Address, Allocation>>,
        escrow_accounts: Receiver<EscrowAccounts>,
        timestamp_error_tolerance: Duration,
        receipt_max_value: u128,
    ) -> Vec<ReceiptCheck> {
        vec![
            Arc::new(AllocationEligible::new(indexer_allocations)),
            Arc::new(SenderBalanceCheck::new(escrow_accounts)),
            Arc::new(TimestampCheck::new(timestamp_error_tolerance)),
            Arc::new(DenyListCheck::new(pgpool.clone()).await),
            Arc::new(ReceiptMaxValueCheck::new(receipt_max_value)),
            Arc::new(MinimumValue::new(pgpool, Duration::from_secs(GRACE_PERIOD)).await),
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
