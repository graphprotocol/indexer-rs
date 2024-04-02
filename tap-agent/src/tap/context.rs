// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
use alloy_primitives::Address;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use sqlx::PgPool;

use super::escrow_adapter::EscrowAdapter;

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
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_adapter: EscrowAdapter,
}

impl TapAgentContext {
    pub fn new(
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_adapter: EscrowAdapter,
    ) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            escrow_adapter,
        }
    }
}
