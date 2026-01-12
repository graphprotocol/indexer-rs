// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, fmt::Debug, sync::Arc, time::Duration};

use indexer_allocation::Allocation;
use indexer_monitor::EscrowAccounts;
use receipt_store::{DatabaseReceipt, InnerContext};
use sqlx::PgPool;
use tap_core::receipt::{checks::ReceiptCheck, state::Checking, ReceiptWithState};
use thegraph_core::alloy::{primitives::Address, sol_types::Eip712Domain};
use tokio::sync::{
    mpsc::{self, Sender},
    watch::Receiver,
};
use tokio_util::sync::CancellationToken;

use crate::tap::checks::{
    allocation_eligible::AllocationEligible, data_service_check::DataServiceCheck,
    deny_list_check::DenyListCheck, receipt_max_val_check::ReceiptMaxValueCheck,
    sender_balance_check::SenderBalanceCheck, timestamp_check::TimestampCheck,
    value_check::MinimumValue,
};

mod checks;
mod receipt_store;

pub use ::indexer_receipt::TapReceipt;
pub use checks::value_check::AgoraQuery;

use self::checks::service_provider::ServiceProviderCheck;

pub type CheckingReceipt = ReceiptWithState<Checking, TapReceipt>;

const GRACE_PERIOD: u64 = 60;

/// Configuration for TAP receipt checks.
pub struct TapChecksConfig {
    pub pgpool: PgPool,
    pub indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    pub escrow_accounts_v1: Option<Receiver<EscrowAccounts>>,
    pub escrow_accounts_v2: Option<Receiver<EscrowAccounts>>,
    pub timestamp_error_tolerance: Duration,
    pub receipt_max_value: u128,
    pub allowed_data_services: Option<Vec<Address>>,
    pub service_provider: Address,
}

#[derive(Clone)]
pub struct IndexerTapContext {
    domain_separator: Arc<Eip712Domain>,
    domain_separator_v2: Arc<Eip712Domain>,
    receipt_producer: Sender<(
        DatabaseReceipt,
        tokio::sync::oneshot::Sender<Result<(), AdapterError>>,
    )>,
    cancelation_token: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error(transparent)]
    AnyhowError(#[from] anyhow::Error),
}

impl IndexerTapContext {
    pub async fn get_checks(config: TapChecksConfig) -> Vec<ReceiptCheck<TapReceipt>> {
        let mut checks: Vec<ReceiptCheck<TapReceipt>> = vec![
            Arc::new(AllocationEligible::new(config.indexer_allocations)),
            Arc::new(SenderBalanceCheck::new(
                config.escrow_accounts_v1,
                config.escrow_accounts_v2,
            )),
            Arc::new(TimestampCheck::new(config.timestamp_error_tolerance)),
            Arc::new(DenyListCheck::new(config.pgpool.clone()).await),
            Arc::new(ReceiptMaxValueCheck::new(config.receipt_max_value)),
            Arc::new(MinimumValue::new(config.pgpool, Duration::from_secs(GRACE_PERIOD)).await),
            Arc::new(ServiceProviderCheck::new(config.service_provider)),
        ];

        if let Some(addrs) = config.allowed_data_services {
            checks.push(Arc::new(DataServiceCheck::new(addrs)));
        }

        checks
    }

    pub async fn new(
        pgpool: PgPool,
        domain_separator: Eip712Domain,
        domain_separator_v2: Eip712Domain,
    ) -> Self {
        const MAX_RECEIPT_QUEUE_SIZE: usize = 1000;
        let (tx, rx) = mpsc::channel(MAX_RECEIPT_QUEUE_SIZE);
        let cancelation_token = CancellationToken::new();
        let inner = InnerContext { pgpool };
        Self::spawn_store_receipt_task(inner, rx, cancelation_token.clone());

        Self {
            cancelation_token,
            receipt_producer: tx,
            domain_separator: Arc::new(domain_separator),
            domain_separator_v2: Arc::new(domain_separator_v2),
        }
    }
}

impl Drop for IndexerTapContext {
    fn drop(&mut self) {
        self.cancelation_token.cancel();
    }
}
