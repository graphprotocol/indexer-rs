// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use anyhow::Error;
use indexer_query::dispute_manager::{self, DisputeManager};
use indexer_watcher::new_watcher;
use thegraph_core::alloy::primitives::Address;
use tokio::sync::watch::Receiver;

use crate::client::SubgraphClient;

/// Watcher for Dispute Manager Address
pub type DisputeManagerWatcher = Receiver<Address>;

/// Monitors the subgraph for dispute manager address
pub async fn dispute_manager(
    network_subgraph: &'static SubgraphClient,
    interval: Duration,
) -> anyhow::Result<DisputeManagerWatcher> {
    new_watcher(interval, move || async move {
        let response = network_subgraph
            .query::<DisputeManager, _>(dispute_manager::Variables {})
            .await?;
        response?
            .graph_network
            .map(|network| network.dispute_manager)
            .ok_or_else(|| Error::msg("Network 1 not found in network subgraph"))
    })
    .await
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use serde_json::json;
    use test_assets::DISPUTE_MANAGER_ADDRESS;
    use tokio::time::sleep;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::client::{DeploymentDetails, SubgraphClient};

    async fn setup_mock_network_subgraph() -> (&'static SubgraphClient, MockServer) {
        // Set up a mock network subgraph
        let mock_server = MockServer::start().await;
        let network_subgraph = SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                &mock_server.uri(),
                test_assets::NETWORK_SUBGRAPH_DEPLOYMENT
            ))
            .unwrap(),
        )
        .await;

        // Mock result for current epoch requests
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(path(format!(
                        "/subgraphs/id/{}",
                        test_assets::NETWORK_SUBGRAPH_DEPLOYMENT
                    )))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "graphNetwork": { "disputeManager": DISPUTE_MANAGER_ADDRESS }}}),
                    )),
            )
            .await;

        (Box::leak(Box::new(network_subgraph)), mock_server)
    }

    #[test_log::test(tokio::test)]
    async fn test_parses_dispute_manager_from_network_subgraph_correctly() {
        let (network_subgraph, _mock_server) = setup_mock_network_subgraph().await;

        let dispute_manager = dispute_manager(network_subgraph, Duration::from_secs(60))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        let result = *dispute_manager.borrow();
        assert_eq!(result, DISPUTE_MANAGER_ADDRESS);
    }
}
