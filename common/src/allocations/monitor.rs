// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eventuals::{timer, Eventual, EventualExt};
use serde::Deserialize;
use thegraph::types::Address;
use tokio::time::sleep;
use tracing::warn;

use crate::prelude::{Query, SubgraphClient};

use super::Allocation;

/// An always up-to-date list of an indexer's active and recently closed allocations.
pub fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
) -> Eventual<HashMap<Address, Allocation>> {
    // Types for deserializing the network subgraph response
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct IndexerAllocationsResponse {
        indexer: Option<Indexer>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Indexer {
        active_allocations: Vec<Allocation>,
        recently_closed_allocations: Vec<Allocation>,
    }

    let query = r#"
        query allocations($indexer: ID!, $closedAtThreshold: Int!) {
            indexer(id: $indexer) {
                activeAllocations: totalAllocations(
                    where: { status: Active }
                    orderDirection: desc
                    first: 1000
                ) {
                    id
                    indexer {
                        id
                    }
                    allocatedTokens
                    createdAtBlockHash
                    createdAtEpoch
                    closedAtEpoch
                    subgraphDeployment {
                        id
                        deniedAt
                    }
                }
                recentlyClosedAllocations: totalAllocations(
                    where: { status: Closed, closedAt_gte: $closedAtThreshold }
                    orderDirection: desc
                    first: 1000
                ) {
                    id
                    indexer {
                        id
                    }
                    allocatedTokens
                    createdAtBlockHash
                    createdAtEpoch
                    closedAtEpoch
                    subgraphDeployment {
                        id
                        deniedAt
                    }
                }
            }
        }
    "#;

    // Refresh indexer allocations every now and then
    timer(interval).map_with_retry(
        move |_| async move {
            // Allocations can be closed 5 mins into the past
            let start = SystemTime::now();
            let since_the_epoch = start
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards");
            const MINUTES_THRESHOLD: u64 = 5;
            let closed_at_threshold = since_the_epoch.as_secs() - MINUTES_THRESHOLD * 60;

            // Query active and recently closed allocations for the indexer,
            // using the network subgraph
            let response = network_subgraph
                .query::<IndexerAllocationsResponse>(Query::new_with_variables(
                    query,
                    [
                        ("indexer", format!("{indexer_address:?}").into()),
                        ("closedAtThreshold", closed_at_threshold.into()),
                    ],
                ))
                .await
                .map_err(|e| e.to_string())?;

            let indexer = response.map_err(|e| e.to_string()).and_then(|data| {
                // Verify that the indexer could be found at all
                data.indexer
                    .ok_or_else(|| format!("Indexer `{indexer_address}` not found on the network"))
            })?;

            // Pull active and recently closed allocations out of the indexer
            let Indexer {
                active_allocations,
                recently_closed_allocations,
            } = indexer;

            Ok(HashMap::from_iter(
                active_allocations
                    .into_iter()
                    .map(|a| (a.id, a))
                    .chain(recently_closed_allocations.into_iter().map(|a| (a.id, a))),
            ))
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

#[cfg(test)]
mod test {
    use wiremock::{
        matchers::{body_string_contains, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use crate::{prelude::SubgraphClient, subgraph_client::DeploymentDetails, test_vectors};

    use super::*;

    async fn setup_mock_network_subgraph() -> (&'static SubgraphClient, MockServer) {
        // Set up a mock network subgraph
        let mock_server = MockServer::start().await;
        let network_subgraph = SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                &mock_server.uri(),
                *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
            ))
            .unwrap(),
        );

        // Mock result for allocations query
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(path(format!(
                        "/subgraphs/id/{}",
                        *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
                    )))
                    .and(body_string_contains("activeAllocations"))
                    .respond_with(ResponseTemplate::new(200).set_body_raw(
                        test_vectors::ALLOCATIONS_QUERY_RESPONSE,
                        "application/json",
                    )),
            )
            .await;

        (Box::leak(Box::new(network_subgraph)), mock_server)
    }

    #[test_log::test(tokio::test)]
    async fn test_parses_allocation_data_from_network_subgraph_correctly() {
        let (network_subgraph, _mock_server) = setup_mock_network_subgraph().await;

        let allocations = indexer_allocations(
            network_subgraph,
            *test_vectors::INDEXER_ADDRESS,
            Duration::from_secs(60),
        );

        assert_eq!(
            allocations.value().await.unwrap(),
            *test_vectors::INDEXER_ALLOCATIONS
        );
    }
}
