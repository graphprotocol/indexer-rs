// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use ethereum_types::U256;
use ethers::types::Address;

use log::{info, warn};
use native::attestation::AttestationSigner;
use tokio::sync::RwLock;

use crate::{
    common::allocation::{allocation_signer, Allocation},
    common::network_subgraph::NetworkSubgraph,
    util::create_attestation_signer,
};

#[derive(Debug)]
struct AllocationMonitorInner {
    network_subgraph: NetworkSubgraph,
    indexer_address: Address,
    interval_ms: u64,
    graph_network_id: u64,
    eligible_allocations: Arc<RwLock<Vec<Allocation>>>,
    attestation_signers: Arc<RwLock<HashMap<Address, AttestationSigner>>>,
    indexer_mnemonic: String,
    chain_id: U256,
    dispute_manager: Address,
}

#[derive(Debug, Clone)]
pub struct AllocationMonitor {
    monitor_handle: Arc<tokio::task::JoinHandle<()>>,
    inner: Arc<AllocationMonitorInner>,
}

impl AllocationMonitor {
    pub async fn new(
        network_subgraph: NetworkSubgraph,
        indexer_address: Address,
        graph_network_id: u64,
        interval_ms: u64,
        indexer_mnemonic: String,
        chain_id: U256,
        dispute_manager: Address,
    ) -> Result<Self> {
        let inner = Arc::new(AllocationMonitorInner {
            network_subgraph,
            indexer_address,
            interval_ms,
            graph_network_id,
            eligible_allocations: Arc::new(RwLock::new(Vec::new())),
            attestation_signers: Arc::new(RwLock::new(HashMap::new())),
            indexer_mnemonic,
            chain_id,
            dispute_manager,
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

    async fn current_epoch(
        network_subgraph: &NetworkSubgraph,
        graph_network_id: u64,
    ) -> Result<u64> {
        let res = network_subgraph
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
        network_subgraph: &NetworkSubgraph,
        indexer_address: &Address,
        closed_at_epoch_threshold: u64,
    ) -> Result<Vec<Allocation>> {
        let res = network_subgraph
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

    async fn update_attestation_signers(inner: &Arc<AllocationMonitorInner>) {
        let mut attestation_signers_write = inner.attestation_signers.write().await;
        for allocation in inner.eligible_allocations.read().await.iter() {
            if let std::collections::hash_map::Entry::Vacant(e) =
                attestation_signers_write.entry(allocation.id)
            {
                match allocation_signer(&inner.indexer_mnemonic, allocation).and_then(|signer| {
                    create_attestation_signer(
                        inner.chain_id,
                        inner.dispute_manager,
                        signer,
                        allocation.subgraph_deployment.id.bytes32(),
                    )
                }) {
                    Ok(signer) => {
                        e.insert(signer);
                        info!(
                            "Found attestation signer for {{allocation: {}, deployment: {}}}",
                            allocation.id,
                            allocation.subgraph_deployment.id.ipfs_hash()
                        );
                    }
                    Err(e) => {
                        warn!(
                        "Failed to find the attestation signer for {{allocation: {}, deployment: {}, createdAtEpoch: {}, err: {}}}",
                        allocation.id, allocation.subgraph_deployment.id.ipfs_hash(), allocation.created_at_epoch, e
                    )
                    }
                }
            }
        }
    }

    async fn monitor_loop(inner: &Arc<AllocationMonitorInner>) -> Result<()> {
        loop {
            let update_allocations_result = Self::update_allocations(inner).await;

            if update_allocations_result.is_err() {
                warn!(
                    "Failed to query indexer allocations, keeping existing: {:?}. Error: {}",
                    inner
                        .eligible_allocations
                        .read()
                        .await
                        .iter()
                        .map(|e| { e.id })
                        .collect::<Vec<Address>>(),
                    update_allocations_result
                        .err()
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

            Self::update_attestation_signers(inner).await;

            tokio::time::sleep(tokio::time::Duration::from_millis(inner.interval_ms)).await;
        }
    }

    pub async fn get_eligible_allocations(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, Vec<Allocation>> {
        self.inner.eligible_allocations.read().await
    }

    pub async fn get_attestation_signers(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, HashMap<Address, AttestationSigner>> {
        self.inner.attestation_signers.read().await
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use test_log::test;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::common::types::SubgraphDeploymentID;
    use crate::common::{
        allocation::{AllocationStatus, SubgraphDeployment},
        network_subgraph::NetworkSubgraph,
    };

    use super::*;

    const INDEXER_OPERATOR_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const INDEXER_ADDRESS: &str = "0x1234567890123456789012345678901234567890";
    const NETWORK_SUBGRAPH_ID: &str = "QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP";
    const DISPUTE_MANAGER_ADDRESS: &str = "0xdeadbeefcafebabedeadbeefcafebabedeadbeef";

    /// The allocation IDs below are generated using the mnemonic
    /// "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
    /// and the following epoch and index:
    ///
    /// - (createdAtEpoch, 0)
    /// - (createdAtEpoch-1, 0)
    /// - (createdAtEpoch, 2)
    /// - (createdAtEpoch-1, 1)
    ///
    /// Using https://github.com/graphprotocol/indexer/blob/f8786c979a8ed0fae93202e499f5ce25773af473/packages/indexer-common/src/allocations/keys.ts#L41-L71
    const ALLOCATIONS_QUERY_RESPONSE: &str = r#"
        {
            "data": {
                "indexer": {
                    "activeAllocations": [
                        {
                            "id": "0xfa44c72b753a66591f241c7dc04e8178c30e13af",
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
                            "id": "0xdd975e30aafebb143e54d215db8a3e8fd916a701",
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
                            "id": "0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658",
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
                            "id": "0x69f961358846fdb64b04e1fd7b2701237c13cd9a",
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
    "#;

    #[test(tokio::test)]
    async fn test_current_epoch() {
        let mock_server = MockServer::start().await;

        let network_subgraph_endpoint =
            NetworkSubgraph::local_deployment_endpoint(&mock_server.uri(), NETWORK_SUBGRAPH_ID);
        let network_subgraph = NetworkSubgraph::new(
            Some(&mock_server.uri()),
            Some(NETWORK_SUBGRAPH_ID),
            network_subgraph_endpoint.as_ref(),
        );

        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + NETWORK_SUBGRAPH_ID))
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
        let indexer_address = Address::from_str(INDEXER_ADDRESS).unwrap();

        let mock_server = MockServer::start().await;

        let network_subgraph_endpoint =
            NetworkSubgraph::local_deployment_endpoint(&mock_server.uri(), NETWORK_SUBGRAPH_ID);
        let network_subgraph = NetworkSubgraph::new(
            Some(&mock_server.uri()),
            Some(NETWORK_SUBGRAPH_ID),
            network_subgraph_endpoint.as_ref(),
        );

        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + NETWORK_SUBGRAPH_ID))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(ALLOCATIONS_QUERY_RESPONSE, "application/json"),
            );

        mock_server.register(mock).await;

        let allocations = AllocationMonitor::current_eligible_allocations(
            &network_subgraph,
            &indexer_address,
            940,
        )
        .await
        .unwrap();

        assert_eq!(allocations.len(), 4);

        assert_eq!(
            allocations[2],
            Allocation {
                id: Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap(),
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

    #[test(tokio::test)]
    async fn test_update_attestation_signers() {
        let indexer_address = Address::from_str(INDEXER_ADDRESS).unwrap();

        let mock_server = MockServer::start().await;

        let network_subgraph_endpoint =
            NetworkSubgraph::local_deployment_endpoint(&mock_server.uri(), NETWORK_SUBGRAPH_ID);
        let network_subgraph = NetworkSubgraph::new(
            Some(&mock_server.uri()),
            Some(NETWORK_SUBGRAPH_ID),
            network_subgraph_endpoint.as_ref(),
        );

        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + NETWORK_SUBGRAPH_ID))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(ALLOCATIONS_QUERY_RESPONSE, "application/json"),
            );

        mock_server.register(mock).await;

        let inner = Arc::new(AllocationMonitorInner {
            network_subgraph: network_subgraph.clone(),
            indexer_address,
            interval_ms: 1000,
            graph_network_id: 1,
            eligible_allocations: Arc::new(RwLock::new(Vec::new())),
            attestation_signers: Arc::new(RwLock::new(HashMap::new())),
            indexer_mnemonic: INDEXER_OPERATOR_MNEMONIC.to_string(),
            chain_id: U256::from(1),
            dispute_manager: Address::from_str(DISPUTE_MANAGER_ADDRESS).unwrap(),
        });

        *(inner.eligible_allocations.write().await) =
            AllocationMonitor::current_eligible_allocations(
                &network_subgraph,
                &indexer_address,
                940,
            )
            .await
            .unwrap();

        AllocationMonitor::update_attestation_signers(&inner).await;

        // Check that the attestation signers were found for the allocations
        assert_eq!(
            inner.attestation_signers.read().await.len(),
            inner.eligible_allocations.read().await.len()
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
        // If you don't specify the correct mnemonic, the attestation signers won't be found
        let indexer_mnemonic = std::env::var("INDEXER_MNEMONIC").expect("INDEXER_MNEMONIC not set");

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
            indexer_mnemonic,
            U256::from(1),
            Address::from_str(DISPUTE_MANAGER_ADDRESS).unwrap(),
        )
        .await
        .unwrap();

        // sleep for a bit to allow the monitor to fetch the allocations a few times
        tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
    }
}
