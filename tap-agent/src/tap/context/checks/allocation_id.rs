// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use alloy_primitives::Address;
use anyhow::anyhow;
use eventuals::{Eventual, EventualExt};
use graphql_client::GraphQLQuery;
use indexer_common::subgraph_client::SubgraphClient;
use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
};
use tokio::time::sleep;
use tracing::error;

use crate::config;

pub struct AllocationId {
    tap_allocation_redeemed: Eventual<bool>,
    allocation_id: Address,
}

impl AllocationId {
    pub fn new(
        sender_id: Address,
        allocation_id: Address,
        escrow_subgraph: &'static SubgraphClient,
        config: &'static config::Cli,
    ) -> Self {
        let tap_allocation_redeemed = tap_allocation_redeemed_eventual(
            allocation_id,
            sender_id,
            config.ethereum.indexer_address,
            escrow_subgraph,
            config.escrow_subgraph.escrow_syncing_interval_ms,
        );

        Self {
            tap_allocation_redeemed,
            allocation_id,
        }
    }
}

#[async_trait::async_trait]
impl Check for AllocationId {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let allocation_id = receipt.signed_receipt().message.allocation_id;
        // TODO: Remove the if block below? Each TAP Monitor is specific to an allocation
        // ID. So the receipts that are received here should already have been filtered by
        // allocation ID.
        if allocation_id != self.allocation_id {
            return Err(anyhow!("Receipt allocation_id different from expected: allocation_id: {}, expected_allocation_id: {}", allocation_id, self.allocation_id));
        };

        // Check that the allocation ID is not redeemed yet for this consumer
        match self.tap_allocation_redeemed.value().await {
            Ok(false) => Ok(()),
            Ok(true) => Err(anyhow!("Allocation {} already redeemed", allocation_id)),
            Err(e) => Err(anyhow!(
                "Could not get allocation escrow redemption status from eventual: {:?}",
                e
            )),
        }
    }
}

fn tap_allocation_redeemed_eventual(
    allocation_id: Address,
    sender_address: Address,
    indexer_address: Address,
    escrow_subgraph: &'static SubgraphClient,
    escrow_subgraph_polling_interval_ms: u64,
) -> Eventual<bool> {
    eventuals::timer(Duration::from_millis(escrow_subgraph_polling_interval_ms)).map_with_retry(
        move |_| async move {
            query_escrow_check_transactions(
                allocation_id,
                sender_address,
                indexer_address,
                escrow_subgraph,
            )
            .await
            .map_err(|e| e.to_string())
        },
        move |error: String| {
            error!(
                "Failed to check the escrow redeem status for allocation {} and sender {}: {}",
                allocation_id, sender_address, error
            );
            sleep(Duration::from_millis(escrow_subgraph_polling_interval_ms).div_f32(2.))
        },
    )
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "../graphql/tap.schema.graphql",
    query_path = "../graphql/transactions.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
struct TapTransactions;

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

    Ok(response.map(|data| !data.transactions.is_empty())?)
}

#[cfg(test)]
mod tests {
    use indexer_common::subgraph_client::{DeploymentDetails, SubgraphClient};

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
        )));

        let result = super::query_escrow_check_transactions(
            allocation_id.parse().unwrap(),
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            escrow_subgraph,
        );

        assert!(result.await.unwrap());
    }
}
