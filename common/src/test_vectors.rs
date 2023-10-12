// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, str::FromStr};

use alloy_primitives::Address;
use ethers_core::types::U256;
use lazy_static::lazy_static;
use toolshed::thegraph::DeploymentId;

use crate::prelude::{Allocation, AllocationStatus, SubgraphDeployment};

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
pub const ALLOCATIONS_QUERY_RESPONSE: &str = r#"
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
                            "deniedAt": 0
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
                            "deniedAt": 0
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
                            "deniedAt": 0
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
                            "deniedAt": 0
                        }
                    }
                ]
            }
        }
    }
"#;

lazy_static! {
    pub static ref NETWORK_SUBGRAPH_DEPLOYMENT: DeploymentId = DeploymentId::from_str("QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP").unwrap();

    pub static ref INDEXER_OPERATOR_MNEMONIC: String = String::from(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    );

    pub static ref INDEXER_ADDRESS: Address =
        Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap();

    pub static ref DISPUTE_MANAGER_ADDRESS: Address =
        Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap();

    /// These are the expected json-serialized contents of the value returned by
    /// AllocationMonitor::current_eligible_allocations with the values above at epoch threshold 940.
    pub static ref INDEXER_ALLOCATIONS: HashMap<Address, Allocation> = HashMap::from([
        (
            Address::from_str("0xfa44c72b753a66591f241c7dc04e8178c30e13af").unwrap(),
            Allocation {
                id: Address::from_str("0xfa44c72b753a66591f241c7dc04e8178c30e13af").unwrap(),
                indexer: Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap(),
                allocated_tokens: U256::from_str("5081382841000000014901161").unwrap(),
                created_at_block_hash:
                    "0x99d3fbdc0105f7ccc0cd5bb287b82657fe92db4ea8fb58242dafb90b1c6e2adf".to_string(),
                created_at_epoch: 953,
                closed_at_epoch: None,
                subgraph_deployment: SubgraphDeployment {
                    id: DeploymentId(
                        "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                            .parse()
                            .unwrap(),
                    ),
                    denied_at: Some(0),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None,
            },
        ),
        (
            Address::from_str("0xdd975e30aafebb143e54d215db8a3e8fd916a701").unwrap(),
            Allocation {
                id: Address::from_str("0xdd975e30aafebb143e54d215db8a3e8fd916a701").unwrap(),
                indexer: Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap(),
                allocated_tokens: U256::from_str("601726452999999979510903").unwrap(),
                created_at_block_hash:
                    "0x99d3fbdc0105f7ccc0cd5bb287b82657fe92db4ea8fb58242dafb90b1c6e2adf".to_string(),
                created_at_epoch: 953,
                closed_at_epoch: None,
                subgraph_deployment: SubgraphDeployment {
                    id: DeploymentId(
                        "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf"
                            .parse()
                            .unwrap(),
                    ),
                    denied_at: Some(0),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None,
            },
        ),
        (
            Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap(),
            Allocation {
                id: Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap(),
                indexer: Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap(),
                allocated_tokens: U256::from_str("5247998688000000081956387").unwrap(),
                created_at_block_hash:
                    "0x6e7b7100c37f659236a029f87ce18914643995120f55ab5d01631f11f40fd887".to_string(),
                created_at_epoch: 940,
                closed_at_epoch: Some(953),
                subgraph_deployment: SubgraphDeployment {
                    id: DeploymentId(
                        "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                            .parse()
                            .unwrap(),
                    ),
                    denied_at: Some(0),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None,
            },
        ),
        (
            Address::from_str("0x69f961358846fdb64b04e1fd7b2701237c13cd9a").unwrap(),
            Allocation {
                id: Address::from_str("0x69f961358846fdb64b04e1fd7b2701237c13cd9a").unwrap(),
                indexer: Address::from_str("0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c").unwrap(),
                allocated_tokens: U256::from_str("2502334654999999795109034").unwrap(),
                created_at_block_hash:
                    "0x6e7b7100c37f659236a029f87ce18914643995120f55ab5d01631f11f40fd887".to_string(),
                created_at_epoch: 940,
                closed_at_epoch: Some(953),
                subgraph_deployment: SubgraphDeployment {
                    id: DeploymentId(
                        "0xc064c354bc21dd958b1d41b67b8ef161b75d2246b425f68ed4c74964ae705cbd"
                            .parse()
                            .unwrap(),
                    ),
                    denied_at: Some(0),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None,
            },
        ),
    ]);
}
