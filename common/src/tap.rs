// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::tap::checks::allocation_eligible::AllocationEligible;
use crate::tap::checks::deny_list_check::DenyListCheck;
use crate::tap::checks::sender_balance_check::SenderBalanceCheck;
use crate::tap::checks::value_check::MinimumValue;
use crate::{escrow_accounts::EscrowAccounts, prelude::Allocation};
use alloy_sol_types::Eip712Domain;
use eventuals::Eventual;
use sqlx::PgPool;
use std::fmt::Debug;
use std::{collections::HashMap, sync::Arc};
use tap_core::receipt::checks::ReceiptCheck;
use thegraph::types::Address;
use tracing::error;

mod checks;
mod receipt_store;

pub use checks::value_check::{
    create_value_check, AgoraQuery, CostModelSource, ValueCheckReceiver, ValueCheckSender,
};

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
        value_check_receiver: ValueCheckReceiver,
    ) -> Vec<ReceiptCheck> {
        vec![
            Arc::new(AllocationEligible::new(indexer_allocations)),
            Arc::new(SenderBalanceCheck::new(
                escrow_accounts.clone(),
                domain_separator.clone(),
            )),
            Arc::new(DenyListCheck::new(pgpool, escrow_accounts, domain_separator).await),
            Arc::new(MinimumValue::new(value_check_receiver)),
        ]
    }

    pub async fn new(pgpool: PgPool, domain_separator: Eip712Domain) -> Self {
        Self {
            pgpool,
            domain_separator: Arc::new(domain_separator),
        }
    }
}
