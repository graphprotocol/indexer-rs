// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_monitor::EscrowAccounts;
use sqlx::PgPool;
use thegraph_core::alloy::primitives::Address;
use tokio::sync::watch::Receiver;

pub mod checks;
mod error;
mod escrow;
mod rav;
mod receipt;

pub use error::AdapterError;

#[derive(Clone)]
pub struct TapAgentContext {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
}

impl TapAgentContext {
    pub fn new(
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        escrow_accounts: Receiver<EscrowAccounts>,
    ) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
        }
    }
}
