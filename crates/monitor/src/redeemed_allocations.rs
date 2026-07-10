// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use indexer_query::redeemed_allocations::{self, RedeemedAllocationsQuery};
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};
use tokio::sync::watch::Receiver;

use crate::{allocations::AllocationWatcher, client::SubgraphClient};

/// How many allocation ids to include per subgraph query.
const ALLOCATION_ID_BATCH_SIZE: usize = 200;

/// Page size for the paginated redeem-transaction query.
const PAGE_SIZE: i64 = 1000;

/// Snapshot of which (payer, allocation) pairs have already had their escrow redeemed.
/// Keyed per payer: one payer redeeming says nothing about other payers on the allocation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RedeemedAllocations(HashMap<Address, HashSet<Address>>);

impl RedeemedAllocations {
    pub fn is_redeemed(&self, payer: &Address, allocation: &Address) -> bool {
        self.0
            .get(payer)
            .is_some_and(|allocations| allocations.contains(allocation))
    }

    pub fn insert(&mut self, payer: Address, allocation: Address) {
        self.0.entry(payer).or_default().insert(allocation);
    }
}

pub type RedeemedAllocationsWatcher = Receiver<RedeemedAllocations>;

pub fn empty_redeemed_allocations_watcher() -> RedeemedAllocationsWatcher {
    let (_, receiver) = tokio::sync::watch::channel(RedeemedAllocations::default());
    receiver
}

/// An always up-to-date set of the redeemed (payer, allocation) pairs among the indexer's
/// active and recently closed allocations. Only allocations in the `allocations` watcher are
/// queried: receipts for any other allocation are already rejected by the eligibility check.
pub async fn redeemed_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    allocations: AllocationWatcher,
    interval: Duration,
) -> Result<RedeemedAllocationsWatcher, anyhow::Error> {
    indexer_watcher::new_watcher(interval, move || {
        let allocations = allocations.clone();
        async move { get_redeemed_allocations(network_subgraph, indexer_address, &allocations).await }
    })
    .await
}

async fn get_redeemed_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    allocations: &AllocationWatcher,
) -> anyhow::Result<RedeemedAllocations> {
    let allocation_ids: Vec<String> = allocations
        .borrow()
        .keys()
        .map(|address| address.encode_hex())
        .collect();

    let mut redeemed = RedeemedAllocations::default();
    if allocation_ids.is_empty() {
        return Ok(redeemed);
    }

    for batch in allocation_ids.chunks(ALLOCATION_ID_BATCH_SIZE) {
        let mut skip: i64 = 0;
        loop {
            let response = network_subgraph
                .query::<RedeemedAllocationsQuery, _>(redeemed_allocations::Variables {
                    receiver: indexer_address.encode_hex(),
                    allocation_ids: Some(batch.to_vec()),
                    first: PAGE_SIZE,
                    skip,
                })
                .await?;

            let page_len = response.payments_escrow_transactions.len() as i64;
            for tx in response.payments_escrow_transactions {
                let Some(allocation_id) = tx.allocation_id else {
                    continue;
                };
                match (
                    Address::from_str(&tx.payer.id),
                    Address::from_str(&allocation_id),
                ) {
                    (Ok(payer), Ok(allocation)) => redeemed.insert(payer, allocation),
                    _ => tracing::warn!(
                        payer = %tx.payer.id,
                        allocation = %allocation_id,
                        "Skipping redeem transaction with unparsable addresses"
                    ),
                }
            }

            if page_len < PAGE_SIZE {
                break;
            }
            skip += PAGE_SIZE;
            tracing::warn!(
                skip,
                "Unexpectedly many redeem transactions for active allocations; fetching next page"
            );
        }
    }

    Ok(redeemed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use test_assets::{ALLOCATION_ID_0, INDEXER_ADDRESS, INDEXER_ALLOCATIONS, TAP_SENDER};
    use test_log::test;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::client::{DeploymentDetails, SubgraphClient};

    async fn mock_network_subgraph(mock_server: &MockServer) -> &'static SubgraphClient {
        Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
            )
            .await,
        ))
    }

    #[test(tokio::test)]
    async fn watcher_reports_redeemed_pairs() {
        let mock_server = MockServer::start().await;
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("paymentsEscrowTransactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "paymentsEscrowTransactions": [{
                                "id": "0x01",
                                "allocationId": ALLOCATION_ID_0.encode_hex(),
                                "payer": { "id": TAP_SENDER.1.encode_hex() },
                                "timestamp": "1"
                            }]
                        }
                    }))),
            )
            .await;
        let network_subgraph = mock_network_subgraph(&mock_server).await;

        let allocations = tokio::sync::watch::channel(INDEXER_ALLOCATIONS.clone()).1;
        let watcher = redeemed_allocations(
            network_subgraph,
            INDEXER_ADDRESS,
            allocations,
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        let snapshot = watcher.borrow().clone();
        assert!(snapshot.is_redeemed(&TAP_SENDER.1, &ALLOCATION_ID_0));
        assert!(!snapshot.is_redeemed(&TAP_SENDER.1, &Address::ZERO));
        assert!(!snapshot.is_redeemed(&Address::ZERO, &ALLOCATION_ID_0));
    }

    #[test(tokio::test)]
    async fn empty_allocation_set_skips_the_query() {
        let mock_server = MockServer::start().await;
        // Expect 0 requests: with no active allocations there is nothing to look up.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(0),
            )
            .await;
        let network_subgraph = mock_network_subgraph(&mock_server).await;

        let allocations = tokio::sync::watch::channel(HashMap::new()).1;
        let watcher = redeemed_allocations(
            network_subgraph,
            INDEXER_ADDRESS,
            allocations,
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        assert_eq!(*watcher.borrow(), RedeemedAllocations::default());
    }
}
