// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::marker::PhantomData;

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

#[sealed::sealed]
pub trait ReceiptType {}

pub enum Legacy {}
pub enum Horizon {}

#[sealed::sealed]
impl ReceiptType for Legacy {}

#[sealed::sealed]
impl ReceiptType for Horizon {}

#[derive(Clone)]
pub struct TapAgentContext<T> {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    _phantom: PhantomData<T>,
}

impl<T: ReceiptType> TapAgentContext<T> {
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
            _phantom: PhantomData,
        }
    }
}
