use std::sync::Arc;

use alloy_primitives::Address;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use sqlx::PgPool;
use tap_core::checks::{ReceiptCheck, TimestampCheck};

use super::escrow_adapter::EscrowAdapter;

pub mod checks;
mod error;
mod escrow;
mod rav;
mod receipt;

pub use error::AdapterError;

#[derive(Clone)]
pub struct TapAgentExecutor {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    required_checks: Vec<ReceiptCheck>,
    escrow_accounts: Eventual<EscrowAccounts>,
    // sender_pending_fees: Arc<RwLock<HashMap<Address, u128>>>,
    escrow_adapter: EscrowAdapter,
    timestamp_check: Arc<TimestampCheck>,
}

impl TapAgentExecutor {
    pub fn new(
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        required_checks: Vec<ReceiptCheck>,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_adapter: EscrowAdapter,
        timestamp_check: Arc<TimestampCheck>,
    ) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
            required_checks,
            escrow_accounts,
            // sender_pending_fees: Arc::new(RwLock::new(HashMap::new())),
            escrow_adapter,
            timestamp_check,
        }
    }
}
