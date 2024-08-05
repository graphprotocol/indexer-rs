// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::tap::checks::allocation_eligible::AllocationEligible;
use crate::tap::checks::deny_list_check::DenyListCheck;
use crate::tap::checks::receipt_max_val_check::ReceiptMaxValueCheck;
use crate::tap::checks::sender_balance_check::SenderBalanceCheck;
use crate::tap::checks::timestamp_check::TimestampCheck;
use crate::{escrow_accounts::EscrowAccounts, prelude::Allocation};
use alloy::dyn_abi::Eip712Domain;
use eventuals::Eventual;
use sqlx::PgPool;
use std::fmt::Debug;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};
use tap_core::receipt::checks::ReceiptCheck;
use thegraph_core::Address;
use tracing::error;

mod checks;
mod receipt_store;

#[derive(Clone)]
pub struct IndexerTapContext {
    pgpool: PgPool,
    domain_separator: Arc<Eip712Domain>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error(transparent)]
    AnyhowError(#[from] anyhow::Error),
}

impl IndexerTapContext {
    pub async fn get_checks(
        pgpool: PgPool,
        indexer_allocations: Eventual<HashMap<Address, Allocation>>,
        escrow_accounts: Eventual<EscrowAccounts>,
        domain_separator: Eip712Domain,
        timestamp_error_tolerance: Duration,
        receipt_max_value: u128,
    ) -> Vec<ReceiptCheck> {
        vec![
            Arc::new(AllocationEligible::new(indexer_allocations)),
            Arc::new(SenderBalanceCheck::new(
                escrow_accounts.clone(),
                domain_separator.clone(),
            )),
            Arc::new(TimestampCheck::new(timestamp_error_tolerance)),
            Arc::new(DenyListCheck::new(pgpool, escrow_accounts, domain_separator).await),
            Arc::new(ReceiptMaxValueCheck::new(receipt_max_value)),
        ]
    }

    pub async fn new(pgpool: PgPool, domain_separator: Eip712Domain) -> Self {
        Self {
            pgpool,
            domain_separator: Arc::new(domain_separator),
        }
    }
}
