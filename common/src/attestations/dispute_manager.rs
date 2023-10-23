// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use alloy_primitives::Address;
use eventuals::{timer, Eventual, EventualExt};
use log::warn;
use serde::Deserialize;
use serde_json::json;
use tokio::time::sleep;

use crate::subgraph_client::SubgraphClient;

pub fn dispute_manager(
    network_subgraph: &'static SubgraphClient,
    graph_network_id: u64,
    interval: Duration,
) -> Eventual<Address> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DisputeManagerResponse {
        graph_network: Option<GraphNetwork>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GraphNetwork {
        dispute_manager: Address,
    }

    timer(interval).map_with_retry(
        move |_| async move {
            let response = network_subgraph
                .query::<DisputeManagerResponse>(&json!({
                    "query": r#"
                        query network($id: ID!) {
                            graphNetwork(id: $id) {
                                disputeManager
                            }
                        }
                    "#,
                    "variables": {
                        "id": graph_network_id
                    }
                }))
                .await
                .map_err(|e| e.to_string())?;

            if let Some(errors) = response.errors {
                warn!(
                    "Errors encountered querying the dispute manager for network {}: {}",
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
                .map(|network| network.dispute_manager)
                .ok_or_else(|| {
                    format!("Network {} not found in network subgraph", graph_network_id)
                })
        },
        move |err: String| {
            warn!(
                "Failed to query dispute manager for network {}: {}",
                graph_network_id, err,
            );

            // Sleep for a bit before we retry
            sleep(interval.div_f32(2.0))
        },
    )
}

#[cfg(test)]
mod test {
    use reqwest::Url;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use crate::{
        prelude::SubgraphClient,
        test_vectors::{self, DISPUTE_MANAGER_ADDRESS},
    };

    use super::*;

    async fn setup_mock_network_subgraph() -> (&'static SubgraphClient, MockServer) {
        // Set up a mock network subgraph
        let mock_server = MockServer::start().await;
        let network_subgraph_endpoint = Url::parse(&format!(
            "{}/subgraphs/id/{}",
            &mock_server.uri(),
            *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
        ))
        .unwrap();
        let network_subgraph =
            SubgraphClient::new("network-subgraph", network_subgraph_endpoint.as_ref()).unwrap();

        // Mock result for current epoch requests
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(path(format!(
                        "/subgraphs/id/{}",
                        *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
                    )))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "graphNetwork": { "disputeManager": *DISPUTE_MANAGER_ADDRESS }}}),
                    )),
            )
            .await;

        (Box::leak(Box::new(network_subgraph)), mock_server)
    }

    #[test_log::test(tokio::test)]
    async fn test_parses_dispute_manager_from_network_subgraph_correctly() {
        let (network_subgraph, _mock_server) = setup_mock_network_subgraph().await;

        let dispute_manager = dispute_manager(network_subgraph, 1, Duration::from_secs(60));

        assert_eq!(
            dispute_manager.value().await.unwrap(),
            *DISPUTE_MANAGER_ADDRESS
        );
    }
}
