// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_monitor::RedeemedAllocationsWatcher;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::CollectionId;

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

/// Rejects receipts whose (payer, allocation) escrow has already been redeemed.
/// Reads a snapshot maintained by a background watcher; performs no I/O per receipt.
pub struct AllocationRedeemedCheck {
    redeemed_allocations: RedeemedAllocationsWatcher,
}

impl AllocationRedeemedCheck {
    pub fn new(redeemed_allocations: RedeemedAllocationsWatcher) -> Self {
        Self {
            redeemed_allocations,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationRedeemedCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(sender) = ctx
            .get::<Sender>()
            .ok_or_else(|| CheckError::Failed(anyhow::anyhow!("Missing sender in context")))?;

        // The network subgraph stores allocationId as the 20-byte address derived
        // from the 32-byte collection_id (rightmost 20 bytes).
        let allocation =
            CollectionId::from(receipt.signed_receipt().as_ref().message.collection_id)
                .as_address();

        if self
            .redeemed_allocations
            .borrow()
            .is_redeemed(sender, &allocation)
        {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Allocation already redeemed (v2): {allocation}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use indexer_monitor::RedeemedAllocations;
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{create_signed_receipt_v2, COLLECTION_ID_0, TAP_SENDER};
    use tokio::sync::watch;

    use super::AllocationRedeemedCheck;
    use crate::{
        middleware::Sender,
        tap::{CheckingReceipt, TapReceipt},
    };

    async fn checking_receipt() -> CheckingReceipt {
        let receipt = create_signed_receipt_v2()
            .collection_id(COLLECTION_ID_0)
            .call()
            .await;
        CheckingReceipt::new(TapReceipt(receipt))
    }

    fn redeemed_snapshot() -> RedeemedAllocations {
        let mut redeemed = RedeemedAllocations::default();
        redeemed.insert(TAP_SENDER.1, COLLECTION_ID_0.as_address());
        redeemed
    }

    #[tokio::test]
    async fn redeemed_allocation_rejects() {
        let (_tx, watcher) = watch::channel(redeemed_snapshot());
        let check = AllocationRedeemedCheck::new(watcher);

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        let result = check.check(&ctx, &checking_receipt().await).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already redeemed"));
    }

    #[tokio::test]
    async fn unredeemed_allocation_passes() {
        let (_tx, watcher) = watch::channel(RedeemedAllocations::default());
        let check = AllocationRedeemedCheck::new(watcher);

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        let result = check.check(&ctx, &checking_receipt().await).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn missing_sender_in_context_rejects() {
        let (_tx, watcher) = watch::channel(redeemed_snapshot());
        let check = AllocationRedeemedCheck::new(watcher);

        let result = check
            .check(&Context::new(), &checking_receipt().await)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing sender"));
    }
}
