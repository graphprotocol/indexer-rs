use std::{collections::HashMap, sync::Arc, time::Duration};

use allocation_eligible::AllocationEligible;
use alloy::primitives::Address;
use indexer_common::allocations::Allocation;
use sqlx::PgPool;
use tap_core::receipt::checks::ReceiptCheck;

mod allocation_eligible;
mod value_check;

use tokio::sync::watch::Receiver;
pub use value_check::AgoraQuery;
use value_check::MinimumValue;

const GRACE_PERIOD: u64 = 60;
pub async fn get_extra_checks(
    pgpool: PgPool,
    indexer_allocations: Receiver<HashMap<Address, Allocation>>,
) -> Vec<ReceiptCheck> {
    vec![
        Arc::new(AllocationEligible::new(indexer_allocations)),
        Arc::new(MinimumValue::new(pgpool, Duration::from_secs(GRACE_PERIOD)).await),
    ]
}
