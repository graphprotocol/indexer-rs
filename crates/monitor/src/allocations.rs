// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::client::SubgraphClient;
use alloy::{primitives::Address, primitives::TxHash};
use indexer_allocation::Allocation;
use indexer_query::allocations_query::{self, AllocationsQuery};
use indexer_watcher::new_watcher;
use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch::Receiver;

/// An always up-to-date list of an indexer's active and recently closed allocations.
pub async fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    recently_closed_allocation_buffer: Duration,
) -> anyhow::Result<Receiver<HashMap<Address, Allocation>>> {
    new_watcher(interval, move || async move {
        get_allocations(
            network_subgraph,
            indexer_address,
            recently_closed_allocation_buffer,
        )
        .await
    })
    .await
}

pub async fn get_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    recently_closed_allocation_buffer: Duration,
) -> Result<HashMap<Address, Allocation>, anyhow::Error> {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let closed_at_threshold = since_the_epoch - recently_closed_allocation_buffer;

    let mut hash: Option<TxHash> = None;
    let mut last: Option<String> = None;
    let mut responses = vec![];
    let page_size = 200;
    loop {
        let result = network_subgraph
            .query::<AllocationsQuery, _>(allocations_query::Variables {
                indexer: indexer_address.to_string().to_ascii_lowercase(),
                closed_at_threshold: closed_at_threshold.as_secs() as i64,
                first: page_size,
                last: last.unwrap_or_default(),
                block: hash.map(|hash| allocations_query::Block_height {
                    hash: Some(hash),
                    number: None,
                    number_gte: None,
                }),
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut data = result?;
        let page_len = data.allocations.len();

        hash = data.meta.and_then(|meta| meta.block.hash);
        last = data.allocations.last().map(|entry| entry.id.to_string());

        responses.append(&mut data.allocations);
        if (page_len as i64) < page_size {
            break;
        }
    }
    let responses = responses
        .into_iter()
        .map(|allocation| allocation.try_into())
        .collect::<Result<Vec<Allocation>, _>>()?;

    Ok(responses
        .into_iter()
        .map(|allocation| (allocation.id, allocation))
        .collect())
}

#[cfg(test)]
mod test {
    const NETWORK_SUBGRAPH_URL: &str =
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-arbitrum";
    use std::str::FromStr;

    use crate::client::{DeploymentDetails, SubgraphClient};

    use super::*;

    async fn network_subgraph_client() -> &'static SubgraphClient {
        Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(NETWORK_SUBGRAPH_URL).unwrap(),
            )
            .await,
        ))
    }

    #[tokio::test]
    #[ignore = "depends on the defunct hosted-service"]
    async fn test_network_query() {
        let result = get_allocations(
            network_subgraph_client().await,
            Address::from_str("0x326c584e0f0eab1f1f83c93cc6ae1acc0feba0bc").unwrap(),
            Duration::from_secs(1712448507),
        )
        .await;
        assert!(result.unwrap().len() > 2000)
    }

    #[tokio::test]
    #[ignore = "depends on the defunct hosted-service"]
    async fn test_network_query_empty_response() {
        let result = get_allocations(
            network_subgraph_client().await,
            Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
            Duration::from_secs(1712448507),
        )
        .await
        .unwrap();
        assert!(result.is_empty())
    }
}
