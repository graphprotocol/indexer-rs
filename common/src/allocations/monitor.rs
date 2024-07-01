// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::Allocation;
use crate::prelude::SubgraphClient;
use eventuals::{timer, Eventual, EventualExt};
use thegraph::types::Address;
use tokio::time::sleep;
use tracing::warn;

/// An always up-to-date list of an indexer's active and recently closed allocations.
pub fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    recently_closed_allocation_buffer: Duration,
) -> Eventual<HashMap<Address, Allocation>> {
    // Refresh indexer allocations every now and then
    timer(interval).map_with_retry(
        move |_| async move {
            get_allocations(
                network_subgraph,
                indexer_address,
                recently_closed_allocation_buffer,
            )
            .await
            .map_err(|e| e.to_string())
        },
        // Need to use string errors here because eventuals `map_with_retry` retries
        // errors that can be cloned
        move |err: String| {
            warn!(
                "Failed to fetch active or recently closed allocations for indexer {:?}: {}",
                indexer_address, err
            );

            // Sleep for a bit before we retry
            sleep(interval.div_f32(2.0))
        },
    )
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

    let query = format!(
        r#"
            allocations(
                block: $block
                orderBy: id
                orderDirection: asc
                first: $first
                where: {{
                and: [
                    {{ id_gt: $last }}
                    {{ indexer_: {{ id: "{}" }} }}
                    {{
                    or: [
                        {{ status: Active }}
                        {{ and: [{{ status: Closed, closedAt_gte: {} }}] }}
                    ]
                    }}
                ]
                }}
            ) {{
                id
                indexer {{
                    id
                }}
                allocatedTokens
                createdAtBlockHash
                createdAtEpoch
                closedAtEpoch
                subgraphDeployment {{
                    id
                    deniedAt
                }}
            }}
        "#,
        indexer_address.to_string().to_ascii_lowercase(),
        closed_at_threshold.as_secs(),
    );
    let responses = network_subgraph
        .paginated_query::<Allocation>(query, 200)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    Ok(HashMap::from_iter(responses.into_iter().map(|a| (a.id, a))))
}

#[cfg(test)]
mod test {
    const NETWORK_SUBGRAPH_URL: &str =
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-arbitrum";
    use std::str::FromStr;

    use crate::{prelude::SubgraphClient, subgraph_client::DeploymentDetails};

    use super::*;

    fn network_subgraph_client() -> &'static SubgraphClient {
        Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(NETWORK_SUBGRAPH_URL, None).unwrap(),
        )))
    }

    #[tokio::test]
    async fn test_network_query() {
        let result = get_allocations(
            network_subgraph_client(),
            Address::from_str("0x326c584e0f0eab1f1f83c93cc6ae1acc0feba0bc").unwrap(),
            Duration::from_secs(1712448507),
        )
        .await;
        assert!(result.unwrap().len() > 2000)
    }

    #[tokio::test]
    async fn test_network_query_empty_response() {
        let result = get_allocations(
            network_subgraph_client(),
            Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
            Duration::from_secs(1712448507),
        )
        .await
        .unwrap();
        assert!(result.is_empty())
    }
}
