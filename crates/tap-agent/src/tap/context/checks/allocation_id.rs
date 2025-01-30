// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use anyhow::anyhow;
use indexer_monitor::SubgraphClient;
use indexer_query::{tap_transactions, TapTransactions};
use indexer_watcher::new_watcher;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::Address;
use tokio::sync::watch::Receiver;

use crate::tap::{CheckingReceipt, TapReceipt};

pub struct AllocationId {
    tap_allocation_redeemed: Receiver<bool>,
    allocation_id: Address,
}

impl AllocationId {
    pub async fn new(
        indexer_address: Address,
        escrow_polling_interval: Duration,
        sender_id: Address,
        allocation_id: Address,
        escrow_subgraph: &'static SubgraphClient,
    ) -> Self {
        let tap_allocation_redeemed = tap_allocation_redeemed_watcher(
            allocation_id,
            sender_id,
            indexer_address,
            escrow_subgraph,
            escrow_polling_interval,
        )
        .await
        .expect("Failed to initialize tap_allocation_redeemed_watcher");

        Self {
            tap_allocation_redeemed,
            allocation_id,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationId {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let allocation_id = receipt.signed_receipt().allocation_id();
        // TODO: Remove the if block below? Each TAP Monitor is specific to an allocation
        // ID. So the receipts that are received here should already have been filtered by
        // allocation ID.
        if allocation_id != self.allocation_id {
            return Err(CheckError::Failed(anyhow!("Receipt allocation_id different from expected: allocation_id: {}, expected_allocation_id: {}", allocation_id, self.allocation_id)));
        };

        // Check that the allocation ID is not redeemed yet for this consumer
        match *self.tap_allocation_redeemed.borrow() {
            false => Ok(()),
            true => Err(CheckError::Failed(anyhow!(
                "Allocation {} already redeemed",
                allocation_id
            ))),
        }
    }
}

async fn tap_allocation_redeemed_watcher(
    allocation_id: Address,
    sender_address: Address,
    indexer_address: Address,
    escrow_subgraph: &'static SubgraphClient,
    escrow_polling_interval: Duration,
) -> anyhow::Result<Receiver<bool>> {
    new_watcher(escrow_polling_interval, move || async move {
        query_escrow_check_transactions(
            allocation_id,
            sender_address,
            indexer_address,
            escrow_subgraph,
        )
        .await
    })
    .await
}

async fn query_escrow_check_transactions(
    allocation_id: Address,
    sender_address: Address,
    indexer_address: Address,
    escrow_subgraph: &'static SubgraphClient,
) -> anyhow::Result<bool> {
    let response = escrow_subgraph
        .query::<TapTransactions, _>(tap_transactions::Variables {
            sender_id: sender_address.to_string().to_lowercase(),
            receiver_id: indexer_address.to_string().to_lowercase(),
            allocation_id: allocation_id.to_string().to_lowercase(),
        })
        .await?;

    response
        .map(|data| !data.transactions.is_empty())
        .map_err(|err| anyhow!(err))
}

#[cfg(test)]
mod tests {
    use indexer_monitor::{DeploymentDetails, SubgraphClient};

    #[tokio::test]
    async fn test_transaction_exists() {
        // testnet values
        let allocation_id = "0x43f8ebe0b6181117eb2dcf8ec7d4e894fca060b8";
        let sender_address = "0x21fed3c4340f67dbf2b78c670ebd1940668ca03e";
        let indexer_address = "0x54d7db28ce0d0e2e87764cd09298f9e4e913e567";

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(
                "https://api.studio.thegraph.com/query/53925/arb-sepolia-tap-subgraph/version/latest"
            )
            .unwrap(),
        ).await
        ));

        let result = super::query_escrow_check_transactions(
            allocation_id.parse().unwrap(),
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            escrow_subgraph,
        );

        assert!(result.await.unwrap());
    }
}
