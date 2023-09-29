// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::Address;
use anyhow::Result;
use log::{info, warn};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch::{Receiver, Sender};
use tokio::sync::RwLock;

use crate::prelude::{Allocation, NetworkSubgraph};

#[derive(Debug)]
struct AllocationMonitorInner {
    network_subgraph: NetworkSubgraph,
    indexer_address: Address,
    interval_ms: u64,
    graph_network_id: u64,
    eligible_allocations: Arc<RwLock<HashMap<Address, Allocation>>>,
    watch_sender: Sender<()>,
    watch_receiver: Receiver<()>,
}

#[cfg_attr(any(test, feature = "mock"), faux::create)]
#[derive(Debug, Clone)]
pub struct AllocationMonitor {
    _monitor_handle: Arc<tokio::task::JoinHandle<()>>,
    inner: Arc<AllocationMonitorInner>,
}

#[cfg_attr(any(test, feature = "mock"), faux::methods)]
impl AllocationMonitor {
    pub async fn new(
        network_subgraph: NetworkSubgraph,
        indexer_address: Address,
        graph_network_id: u64,
        interval_ms: u64,
    ) -> Result<Self> {
        // These are used to ping subscribers when the allocations are updated
        let (watch_sender, watch_receiver) = tokio::sync::watch::channel(());

        let inner = Arc::new(AllocationMonitorInner {
            network_subgraph,
            indexer_address,
            interval_ms,
            graph_network_id,
            eligible_allocations: Arc::new(RwLock::new(HashMap::new())),
            watch_sender,
            watch_receiver,
        });

        let inner_clone = inner.clone();

        let monitor = AllocationMonitor {
            _monitor_handle: Arc::new(tokio::spawn(async move {
                AllocationMonitor::monitor_loop(&inner_clone).await.unwrap();
            })),
            inner,
        };

        Ok(monitor)
    }

    async fn current_epoch(
        network_subgraph: &NetworkSubgraph,
        graph_network_id: u64,
    ) -> Result<u64> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GraphNetwork {
            current_epoch: u64,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct QueryResult {
            graph_network: GraphNetwork,
        }

        let res = network_subgraph
            .query::<QueryResult>(&json!({
                "query": r#"
                    query epoch($id: ID!) {
                        graphNetwork(id: $id) {
                            currentEpoch
                        }
                    }
                "#,
                "variables": { "id": graph_network_id },
            }))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse current epoch response from network subgraph: {}",
                    e
                )
            })?;

        res.data
            .map(|data| data.graph_network.current_epoch)
            .ok_or(anyhow::anyhow!(
                "Failed to get current epoch from network subgraph"
            ))
    }

    async fn current_eligible_allocations(
        network_subgraph: &NetworkSubgraph,
        indexer_address: &Address,
        closed_at_epoch_threshold: u64,
    ) -> Result<HashMap<Address, Allocation>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Indexer {
            active_allocations: Vec<Allocation>,
            recently_closed_allocations: Vec<Allocation>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct QueryResult {
            indexer: Option<Indexer>,
        }

        let result = network_subgraph
            .query::<QueryResult>(&json!({
                "query": r#"
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
                                    stakedTokens
                                    signalledTokens
                                    queryFeesAmount
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
                                    stakedTokens
                                    signalledTokens
                                    queryFeesAmount
                                }
                            }
                        }
                    }
                "#,
                "variables": {
                    "indexer": indexer_address,
                    "closedAtEpochThreshold": closed_at_epoch_threshold
                }
            }))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to fetch current allocations for indexer {} from network subgraph: {}",
                    indexer_address, e
                )
            })?;

        let indexer = result.data.and_then(|d| d.indexer).ok_or_else(|| {
            anyhow::anyhow!("No data / indexer {} not found on chain", indexer_address)
        })?;

        Ok(HashMap::from_iter(
            indexer
                .active_allocations
                .into_iter()
                .chain(indexer.recently_closed_allocations.into_iter())
                .map(|allocation| (allocation.id, allocation)),
        ))
    }

    async fn update_allocations(inner: &Arc<AllocationMonitorInner>) -> Result<(), anyhow::Error> {
        let current_epoch =
            Self::current_epoch(&inner.network_subgraph, inner.graph_network_id).await?;
        *(inner.eligible_allocations.write().await) = Self::current_eligible_allocations(
            &inner.network_subgraph,
            &inner.indexer_address,
            current_epoch - 1,
        )
        .await?;
        Ok(())
    }

    async fn monitor_loop(inner: &Arc<AllocationMonitorInner>) -> Result<()> {
        loop {
            match Self::update_allocations(inner).await {
                Ok(_) => {
                    if inner.watch_sender.send(()).is_err() {
                        warn!(
                            "Failed to notify subscribers that the allocations have been updated"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to query indexer allocations, keeping existing: {:?}. Error: {}",
                        inner
                            .eligible_allocations
                            .read()
                            .await
                            .keys()
                            .collect::<Vec<&Address>>(),
                        e
                    );
                }
            }

            info!(
                "Eligible allocations: {}",
                inner
                    .eligible_allocations
                    .read()
                    .await
                    .values()
                    .map(|e| {
                        format!(
                            "{{allocation: {:?}, deployment: {}, closedAtEpoch: {:?})}}",
                            e.id,
                            e.subgraph_deployment.id.ipfs_hash(),
                            e.closed_at_epoch
                        )
                    })
                    .collect::<Vec<String>>()
                    .join(", ")
            );

            tokio::time::sleep(tokio::time::Duration::from_millis(inner.interval_ms)).await;
        }
    }

    pub async fn get_eligible_allocations(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, HashMap<Address, Allocation>> {
        self.inner.eligible_allocations.read().await
    }

    pub async fn is_allocation_eligible(&self, allocation_id: &Address) -> bool {
        self.inner
            .eligible_allocations
            .read()
            .await
            .contains_key(allocation_id)
    }

    pub fn subscribe(&self) -> Receiver<()> {
        self.inner.watch_receiver.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use test_log::test;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::prelude::NetworkSubgraph;
    use crate::test_vectors;

    use super::*;

    #[test(tokio::test)]
    async fn test_current_epoch() {
        let mock_server = MockServer::start().await;

        let network_subgraph_endpoint = NetworkSubgraph::local_deployment_endpoint(
            &mock_server.uri(),
            test_vectors::NETWORK_SUBGRAPH_ID,
        );
        let network_subgraph = NetworkSubgraph::new(
            Some(&mock_server.uri()),
            Some(test_vectors::NETWORK_SUBGRAPH_ID),
            network_subgraph_endpoint.as_ref(),
        );

        let mock = Mock::given(method("POST"))
            .and(path(
                "/subgraphs/id/".to_string() + test_vectors::NETWORK_SUBGRAPH_ID,
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"
                    {
                        "data": {
                            "graphNetwork": {
                                "currentEpoch": 896419
                            }
                        }
                    }
                "#,
                "application/json",
            ));

        mock_server.register(mock).await;

        let epoch = AllocationMonitor::current_epoch(&network_subgraph, 1)
            .await
            .unwrap();

        assert_eq!(epoch, 896419);
    }

    #[test(tokio::test)]
    async fn test_current_eligible_allocations() {
        let indexer_address = Address::from_str(test_vectors::INDEXER_ADDRESS).unwrap();

        let mock_server = MockServer::start().await;

        let network_subgraph_endpoint = NetworkSubgraph::local_deployment_endpoint(
            &mock_server.uri(),
            test_vectors::NETWORK_SUBGRAPH_ID,
        );
        let network_subgraph = NetworkSubgraph::new(
            Some(&mock_server.uri()),
            Some(test_vectors::NETWORK_SUBGRAPH_ID),
            network_subgraph_endpoint.as_ref(),
        );

        let mock = Mock::given(method("POST"))
            .and(path(
                "/subgraphs/id/".to_string() + test_vectors::NETWORK_SUBGRAPH_ID,
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_vectors::ALLOCATIONS_QUERY_RESPONSE, "application/json"),
            );

        mock_server.register(mock).await;

        let allocations = AllocationMonitor::current_eligible_allocations(
            &network_subgraph,
            &indexer_address,
            940,
        )
        .await
        .unwrap();

        assert_eq!(allocations, test_vectors::expected_eligible_allocations())
    }

    /// Run with RUST_LOG=info to see the logs from the allocation monitor
    #[test(tokio::test)]
    #[ignore]
    async fn test_local() {
        let graph_node_url =
            std::env::var("GRAPH_NODE_ENDPOINT").expect("GRAPH_NODE_ENDPOINT not set");
        let network_subgraph_id =
            std::env::var("NETWORK_SUBGRAPH_ID").expect("NETWORK_SUBGRAPH_ID not set");
        let indexer_address = std::env::var("INDEXER_ADDRESS").expect("INDEXER_ADDRESS not set");

        let network_subgraph_endpoint =
            NetworkSubgraph::local_deployment_endpoint(&graph_node_url, &network_subgraph_id);
        let network_subgraph = NetworkSubgraph::new(
            Some(&graph_node_url),
            Some(&network_subgraph_id),
            network_subgraph_endpoint.as_ref(),
        );

        // graph_network_id=1 and interval_ms=1000
        let _allocation_monitor = AllocationMonitor::new(
            network_subgraph,
            Address::from_str(&indexer_address).unwrap(),
            1,
            1000,
        )
        .await
        .unwrap();

        // sleep for a bit to allow the monitor to fetch the allocations a few times
        tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
    }
}
