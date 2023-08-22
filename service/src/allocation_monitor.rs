// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::Result;
use ethers::types::Address;

use log::{info, warn};
use tokio::sync::RwLock;

use crate::{common::allocation::Allocation, graph_node::GraphNodeInstance};

struct AllocationMonitorInner {
    graph_node: GraphNodeInstance,
    indexer_address: Address,
    interval_ms: u64,
    graph_network_id: u64,
    eligible_allocations: Arc<RwLock<Vec<Allocation>>>,
}

#[derive(Clone)]
pub struct AllocationMonitor {
    monitor_handle: Arc<tokio::task::JoinHandle<()>>,
    inner: Arc<AllocationMonitorInner>,
}

impl AllocationMonitor {
    pub async fn new(
        graph_node: GraphNodeInstance,
        indexer_address: Address,
        graph_network_id: u64,
        interval_ms: u64,
    ) -> Result<Self> {
        let inner = Arc::new(AllocationMonitorInner {
            graph_node,
            indexer_address,
            interval_ms,
            graph_network_id,
            eligible_allocations: Arc::new(RwLock::new(Vec::new())),
        });

        let inner_clone = inner.clone();

        let monitor = AllocationMonitor {
            monitor_handle: Arc::new(tokio::spawn(async move {
                AllocationMonitor::monitor_loop(&inner_clone).await.unwrap();
            })),
            inner,
        };

        Ok(monitor)
    }

    async fn current_epoch(graph_node: &GraphNodeInstance, graph_network_id: u64) -> Result<u64> {
        let res = graph_node
            .network_query(
                r#"
                    query epoch($id: ID!) {
                        graphNetwork(id: $id) {
                            currentEpoch
                        }
                    }
                "#
                .to_string(),
                Some(serde_json::json!({ "id": graph_network_id })),
            )
            .await?;

        let res_json: serde_json::Value = serde_json::from_str(res.graphql_response.as_str())
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse current epoch response from network subgraph: {}",
                    e
                )
            })?;

        res_json
            .get("data")
            .and_then(|d| d.get("graphNetwork"))
            .and_then(|d| d.get("currentEpoch"))
            .and_then(|d| d.as_u64())
            .ok_or(anyhow::anyhow!(
                "Failed to get current epoch from network subgraph"
            ))
    }

    async fn current_eligible_allocations(
        graph_node: &GraphNodeInstance,
        indexer_address: &Address,
        closed_at_epoch_threshold: u64,
    ) -> Result<Vec<Allocation>> {
        let res = graph_node
        .network_query(
            r#"
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
            "#
            .to_string(),
            Some(serde_json::json!({ "indexer": indexer_address, "closedAtEpochThreshold": closed_at_epoch_threshold })),
        )
        .await;

        let mut res_json: serde_json::Value = serde_json::from_str(res?.graphql_response.as_str())
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to fetch current allocations from network subgraph: {}",
                    e
                )
            })?;

        let mut eligible_allocations: Vec<Allocation> = Vec::new();

        let indexer_json = res_json
            .get_mut("data")
            .and_then(|d| d.get_mut("indexer"))
            .ok_or_else(|| anyhow::anyhow!("No data / indexer not found on chain",))?;

        let active_allocations_json =
            indexer_json.get_mut("activeAllocations").ok_or_else(|| {
                anyhow::anyhow!("Failed to parse active allocations from network subgraph",)
            })?;
        eligible_allocations.append(&mut serde_json::from_value(active_allocations_json.take())?);

        let recently_closed_allocations_json =
            indexer_json
                .get_mut("recentlyClosedAllocations")
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Failed to parse recently closed allocations from network subgraph",
                    )
                })?;
        eligible_allocations.append(&mut serde_json::from_value(
            recently_closed_allocations_json.take(),
        )?);

        Ok(eligible_allocations)
    }

    async fn update(inner: &Arc<AllocationMonitorInner>) -> Result<(), anyhow::Error> {
        let current_epoch = Self::current_epoch(&inner.graph_node, inner.graph_network_id).await?;
        *(inner.eligible_allocations.write().await) = Self::current_eligible_allocations(
            &inner.graph_node,
            &inner.indexer_address,
            current_epoch - 1,
        )
        .await?;
        Ok(())
    }

    async fn monitor_loop(inner: &Arc<AllocationMonitorInner>) -> Result<()> {
        loop {
            let res = Self::update(inner).await;

            if res.is_err() {
                warn!(
                    "Failed to query indexer allocations, keeping existing: {:?}. Error: {}",
                    inner
                        .eligible_allocations
                        .read()
                        .await
                        .iter()
                        .map(|e| { e.id })
                        .collect::<Vec<Address>>(),
                    res.err()
                        .unwrap_or_else(|| anyhow::anyhow!("Unknown error"))
                );
            }

            info!(
                "Eligible allocations: {}",
                inner
                    .eligible_allocations
                    .read()
                    .await
                    .iter()
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
    ) -> tokio::sync::RwLockReadGuard<'_, Vec<Allocation>> {
        self.inner.eligible_allocations.read().await
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethereum_types::U256;
    use test_log::test;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::common::allocation::{AllocationStatus, SubgraphDeployment};
    use crate::common::types::SubgraphDeploymentID;
    use crate::graph_node::GraphNodeInstance;

    #[test(tokio::test)]
    async fn test_current_epoch() {
        let network_subgraph_id = "QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP";
        let _indexer_address =
            Address::from_str("0x1234567890123456789012345678901234567890").unwrap();

        let mock_server = MockServer::start().await;

        let graph_node = GraphNodeInstance::new(&mock_server.uri(), network_subgraph_id);

        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + network_subgraph_id))
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

        let epoch = AllocationMonitor::current_epoch(&graph_node, 1)
            .await
            .unwrap();

        assert_eq!(epoch, 896419);
    }

    #[test(tokio::test)]
    async fn test_current_eligible_allocations() {
        let network_subgraph_id = "QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP";
        let indexer_address =
            Address::from_str("0x1234567890123456789012345678901234567890").unwrap();

        let mock_server = MockServer::start().await;

        let graph_node = GraphNodeInstance::new(&mock_server.uri(), network_subgraph_id);

        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + network_subgraph_id))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"
                    {
                        "data": {
                            "indexer": {
                                "activeAllocations": [
                                    {
                                        "id": "0x0b1565a827f4b92849b17de9e3ca1fa1cc51e9a7",
                                        "indexer": {
                                            "id": "0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
                                        },
                                        "allocatedTokens": "5081382841000000014901161",
                                        "createdAtBlockHash": "0x99d3fbdc0105f7ccc0cd5bb287b82657fe92db4ea8fb58242dafb90b1c6e2adf",
                                        "createdAtEpoch": 953,
                                        "closedAtEpoch": null,
                                        "subgraphDeployment": {
                                            "id": "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                                            "deniedAt": 0,
                                            "stakedTokens": "96183284152000000014901161",
                                            "signalledTokens": "182832939554154667498047",
                                            "queryFeesAmount": "19861336072168874330350"
                                        }
                                    },
                                    {
                                        "id": "0x10ff1b029f28bfb8ec4badd5672478f27d574b7e",
                                        "indexer": {
                                            "id": "0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
                                        },
                                        "allocatedTokens": "601726452999999979510903",
                                        "createdAtBlockHash": "0x99d3fbdc0105f7ccc0cd5bb287b82657fe92db4ea8fb58242dafb90b1c6e2adf",
                                        "createdAtEpoch": 953,
                                        "closedAtEpoch": null,
                                        "subgraphDeployment": {
                                            "id": "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf",
                                            "deniedAt": 0,
                                            "stakedTokens": "53885041676589999979510903",
                                            "signalledTokens": "104257136417832003117925",
                                            "queryFeesAmount": "2229358609434396563687"
                                        }
                                    }
                                ],
                                "recentlyClosedAllocations": [
                                    {
                                        "id": "0xc6aea0f1271bca4629d21e33be4dc3c070538f05",
                                        "indexer": {
                                            "id": "0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
                                        },
                                        "allocatedTokens": "5247998688000000081956387",
                                        "createdAtBlockHash": "0x6e7b7100c37f659236a029f87ce18914643995120f55ab5d01631f11f40fd887",
                                        "createdAtEpoch": 940,
                                        "closedAtEpoch": 953,
                                        "subgraphDeployment": {
                                            "id": "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                                            "deniedAt": 0,
                                            "stakedTokens": "96183284152000000014901161",
                                            "signalledTokens": "182832939554154667498047",
                                            "queryFeesAmount": "19861336072168874330350"
                                        }
                                    },
                                    {
                                        "id": "0xe255ecd4b78b1aeb7e5acf7d6dc43b4823c4461c",
                                        "indexer": {
                                            "id": "0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
                                        },
                                        "allocatedTokens": "2502334654999999795109034",
                                        "createdAtBlockHash": "0x6e7b7100c37f659236a029f87ce18914643995120f55ab5d01631f11f40fd887",
                                        "createdAtEpoch": 940,
                                        "closedAtEpoch": 953,
                                        "subgraphDeployment": {
                                            "id": "0xc064c354bc21dd958b1d41b67b8ef161b75d2246b425f68ed4c74964ae705cbd",
                                            "deniedAt": 0,
                                            "stakedTokens": "85450761241000000055879354",
                                            "signalledTokens": "154944508746646550301048",
                                            "queryFeesAmount": "4293718622418791971020"
                                        }
                                    }
                                ]
                            }
                        }
                    }
                "#,
                "application/json",
            ));

        mock_server.register(mock).await;

        let allocations =
            AllocationMonitor::current_eligible_allocations(&graph_node, &indexer_address, 940)
                .await
                .unwrap();

        assert_eq!(allocations.len(), 4);

        assert_eq!(
            allocations[2],
            Allocation {
                id: Address::from_str("0xc6aea0f1271bca4629d21e33be4dc3c070538f05").unwrap(),
                indexer: Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap(),
                allocated_tokens: U256::from_str("5247998688000000081956387").unwrap(),
                created_at_block_hash:
                    "0x6e7b7100c37f659236a029f87ce18914643995120f55ab5d01631f11f40fd887".to_string(),
                created_at_epoch: 940,
                closed_at_epoch: Some(953),
                subgraph_deployment: SubgraphDeployment {
                    id: SubgraphDeploymentID::new(
                        "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                    )
                    .unwrap(),
                    denied_at: Some(0),
                    staked_tokens: U256::from_str("96183284152000000014901161").unwrap(),
                    signalled_tokens: U256::from_str("182832939554154667498047").unwrap(),
                    query_fees_amount: U256::from_str("19861336072168874330350").unwrap(),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None
            }
        );
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

        let graph_node = GraphNodeInstance::new(&graph_node_url, &network_subgraph_id);

        // graph_network_id=1 and interval_ms=1000
        let _allocation_monitor = AllocationMonitor::new(
            graph_node,
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
