// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, time::Duration};

use alloy_primitives::Address;
use anyhow::anyhow;
use eventuals::{timer, Eventual, EventualExt};
use log::warn;
use serde::Deserialize;
use serde_json::json;
use tokio::time::sleep;

use crate::prelude::SubgraphClient;

use super::Allocation;

async fn current_epoch(
    network_subgraph: &'static SubgraphClient,
    graph_network_id: u64,
) -> Result<u64, anyhow::Error> {
    // Types for deserializing the network subgraph response
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GraphNetworkResponse {
        graph_network: Option<GraphNetwork>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GraphNetwork {
        current_epoch: u64,
    }

    // Query the current epoch
    let query = r#"query epoch($id: ID!) { graphNetwork(id: $id) { currentEpoch } }"#;
    let response = network_subgraph
        .query::<GraphNetworkResponse>(&json!({
            "query": query,
            "variables": {
                "id": graph_network_id
            }
        }))
        .await?;

    if let Some(errors) = response.errors {
        warn!(
            "Errors encountered identifying current epoch for network {}: {}",
            graph_network_id,
            errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    response
        .data
        .and_then(|data| data.graph_network)
        .ok_or_else(|| anyhow!("Network {} not found", graph_network_id))
        .map(|network| network.current_epoch)
}

/// An always up-to-date list of an indexer's active and recently closed allocations.
pub fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    graph_network_id: u64,
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
        query allocations($indexer: ID!, $closedAtEpochThreshold: Int!) {
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
                    where: { status: Closed, closedAtEpoch_gte: $closedAtEpochThreshold }
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
            let current_epoch = current_epoch(network_subgraph, graph_network_id)
                .await
                .map_err(|e| format!("Failed to fetch current epoch: {}", e))?;

            // Allocations can be closed one epoch into the past
            let closed_at_epoch_threshold = current_epoch - 1;

            // Query active and recently closed allocations for the indexer,
            // using the network subgraph
            let response = network_subgraph
                .query::<IndexerAllocationsResponse>(&json!({
                "query": query,
                "variables": {
                    "indexer": format!("{indexer_address:?}"),
                    "closedAtEpochThreshold": closed_at_epoch_threshold,
                }}))
                .await
                .map_err(|e| e.to_string())?;

            // If there are any GraphQL errors returned, we'll log them for debugging
            if let Some(errors) = response.errors {
                warn!(
                    "Errors encountered fetching active or recently closed allocations for indexer {:?}: {}",
                    indexer_address,
                    errors.into_iter().map(|e| e.message).collect::<Vec<_>>().join(", ")
                );
            }

            // Verify that the indexer could be found at all
            let indexer = response
                .data
                .and_then(|data| data.indexer)
                .ok_or_else(|| format!("Indexer {:?} could not be found on the network", indexer_address))?;

            // Pull active and recently closed allocations out of the indexer
            let Indexer {
                active_allocations,
                recently_closed_allocations
            } = indexer;

            Ok(HashMap::from_iter(
                active_allocations.into_iter().map(|a| (a.id, a)).chain(
                    recently_closed_allocations.into_iter().map(|a| (a.id, a)))
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
    use serde_json::json;
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
            None,
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                &mock_server.uri(),
                *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
            ))
            .unwrap(),
        )
        .unwrap();

        // Mock result for current epoch requests
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(path(format!(
                        "/subgraphs/id/{}",
                        *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
                    )))
                    .and(body_string_contains("currentEpoch"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "graphNetwork": { "currentEpoch": 896419 }}}),
                    )),
            )
            .await;

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
            1,
            Duration::from_secs(60),
        );

        assert_eq!(
            allocations.value().await.unwrap(),
            *test_vectors::INDEXER_ALLOCATIONS
        );
    }
}
