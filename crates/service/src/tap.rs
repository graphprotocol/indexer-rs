// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # TAP Receipt Processing
//!
//! This module handles Timeline Aggregation Protocol (TAP) receipt validation,
//! storage, and check execution for the indexer service.
//!
//! ## Overview
//!
//! TAP is a payment protocol that enables efficient micropayments for GraphQL queries.
//! Gateways attach signed receipts to each query, and the indexer validates and stores
//! these receipts for later aggregation into Receipt Aggregate Vouchers (RAVs).
//!
//! ## Receipt Types
//!
//! The system supports two receipt versions:
//! - **V1 (Legacy)**: Allocation-based receipts tied to specific allocations
//! - **V2 (Horizon)**: Collection-based receipts using the Horizon payment contracts
//!
//! ## Validation Checks
//!
//! Receipts pass through a series of validation checks before being stored:
//!
//! 1. [`AllocationEligible`](checks::allocation_eligible::AllocationEligible) -
//!    Verifies the allocation/collection exists and is active
//! 2. [`AllocationRedeemedCheck`](checks::allocation_redeemed::AllocationRedeemedCheck) -
//!    Ensures the allocation hasn't been closed/redeemed
//! 3. [`SenderBalanceCheck`](checks::sender_balance_check::SenderBalanceCheck) -
//!    Confirms sender has sufficient escrow balance
//! 4. [`TimestampCheck`](checks::timestamp_check::TimestampCheck) -
//!    Validates receipt timestamp is within acceptable bounds
//! 5. [`DenyListCheck`](checks::deny_list_check::DenyListCheck) -
//!    Rejects receipts from denied senders
//! 6. [`ReceiptMaxValueCheck`](checks::receipt_max_val_check::ReceiptMaxValueCheck) -
//!    Caps maximum receipt value to prevent abuse
//! 7. [`MinimumValue`](checks::value_check::MinimumValue) -
//!    Ensures receipt meets minimum cost model requirements
//! 8. [`ServiceProviderCheck`](checks::service_provider::ServiceProviderCheck) -
//!    Verifies the service provider matches the indexer
//! 9. [`PayerCheck`](checks::payer_check::PayerCheck) -
//!    Validates payer field for V2 receipts
//! 10. [`DataServiceCheck`](checks::data_service_check::DataServiceCheck) -
//!     Optional check for allowed data services (V2)
//!
//! ## Storage Pipeline
//!
//! Valid receipts are queued for batch storage via an async channel. The
//! [`IndexerTapContext`] manages this pipeline with configurable queue size
//! and batch processing for database efficiency.

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

use crate::{
    constants::{TAP_RECEIPT_GRACE_PERIOD, TAP_RECEIPT_MAX_QUEUE_SIZE},
    tap::checks::{
        allocation_eligible::AllocationEligible, allocation_redeemed::AllocationRedeemedCheck,
        data_service_check::DataServiceCheck, deny_list_check::DenyListCheck,
        payer_check::PayerCheck, receipt_max_val_check::ReceiptMaxValueCheck,
        sender_balance_check::SenderBalanceCheck, timestamp_check::TimestampCheck,
        value_check::MinimumValue,
    },
};

mod checks;
mod receipt_store;

pub use ::indexer_receipt::TapReceipt;
pub use checks::value_check::AgoraQuery;

use self::checks::service_provider::ServiceProviderCheck;

pub type CheckingReceipt = ReceiptWithState<Checking, TapReceipt>;

/// Configuration for TAP receipt checks.
pub struct TapChecksConfig {
    pub pgpool: PgPool,
    pub indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    pub escrow_accounts_v2: Option<Receiver<EscrowAccounts>>,
    pub network_subgraph: Option<&'static indexer_monitor::SubgraphClient>,
    pub indexer_address: Address,
    pub timestamp_error_tolerance: Duration,
    pub receipt_max_value: u128,
    pub allowed_data_services: Option<Vec<Address>>,
    pub service_provider: Address,
}

#[derive(Clone)]
pub struct IndexerTapContext {
    domain_separator_v2: Arc<Eip712Domain>,
    receipt_producer: Sender<(
        DatabaseReceipt,
        tokio::sync::oneshot::Sender<Result<(), AdapterError>>,
    )>,
    cancelation_token: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("Database operation failed")]
    Database(#[source] sqlx::Error),

    #[error("Failed to recover signer from receipt")]
    SignerRecovery(#[source] tap_core::signed_message::Eip712Error),

    #[error("Failed to queue receipt for storage: channel closed")]
    ChannelSend,

    #[error("Failed to receive storage result")]
    ChannelRecv(#[source] tokio::sync::oneshot::error::RecvError),
}

impl IndexerTapContext {
    pub async fn get_checks(config: TapChecksConfig) -> Vec<ReceiptCheck<TapReceipt>> {
        let mut checks: Vec<ReceiptCheck<TapReceipt>> = vec![
            Arc::new(AllocationEligible::new(config.indexer_allocations)),
            Arc::new(AllocationRedeemedCheck::new(
                config.indexer_address,
                config.network_subgraph,
            )),
            Arc::new(SenderBalanceCheck::new(config.escrow_accounts_v2)),
            Arc::new(TimestampCheck::new(config.timestamp_error_tolerance)),
            Arc::new(DenyListCheck::new(config.pgpool.clone()).await),
            Arc::new(ReceiptMaxValueCheck::new(config.receipt_max_value)),
            Arc::new(MinimumValue::new(config.pgpool, TAP_RECEIPT_GRACE_PERIOD).await),
            Arc::new(ServiceProviderCheck::new(config.service_provider)),
            Arc::new(PayerCheck::new()),
        ];

        if let Some(addrs) = config.allowed_data_services {
            checks.push(Arc::new(DataServiceCheck::new(addrs)));
        }

        checks
    }

    pub async fn new(pgpool: PgPool, domain_separator_v2: Eip712Domain) -> Self {
        let (tx, rx) = mpsc::channel(TAP_RECEIPT_MAX_QUEUE_SIZE);
        let cancelation_token = CancellationToken::new();
        let inner = InnerContext { pgpool };
        Self::spawn_store_receipt_task(inner, rx, cancelation_token.clone());

        Self {
            cancelation_token,
            receipt_producer: tx,
            domain_separator_v2: Arc::new(domain_separator_v2),
        }
    }
}

impl Drop for IndexerTapContext {
    fn drop(&mut self) {
        self.cancelation_token.cancel();
    }
}
