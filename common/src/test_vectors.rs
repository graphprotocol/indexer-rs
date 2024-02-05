// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, str::FromStr};

use alloy_sol_types::{eip712_domain, Eip712Domain};
use ethers::signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use ethers_core::types::U256;
use lazy_static::lazy_static;
use tap_core::{
    eip_712_signed_message::EIP712SignedMessage, tap_manager::SignedReceipt, tap_receipt::Receipt,
};
use thegraph::types::Address;
use thegraph::types::DeploymentId;

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

pub const ESCROW_QUERY_RESPONSE: &str = r#"
    {
        "data": {
            "escrowAccounts": [
                {
                    "balance": "34",
                    "totalAmountThawing": "10",
                    "sender": {
                        "id": "0x9858EfFD232B4033E47d90003D41EC34EcaEda94",
                        "authorizedSigners": [
                            {
                                "id": "0x533661F0fb14d2E8B26223C86a610Dd7D2260892"
                            },
                            {
                                "id": "0x2740f6fA9188cF53ffB6729DDD21575721dE92ce"
                            }
                        ]
                    }
                },
                {
                    "balance": "42",
                    "totalAmountThawing": "0",
                    "sender": {
                        "id": "0x22d491bde2303f2f43325b2108d26f1eaba1e32b",
                        "authorizedSigners": [
                            {
                                "id": "0x245059163ff6ee14279aa7b35ea8f0fdb967df6e"
                            }
                        ]
                    }
                },
                {
                    "balance": "2987",
                    "totalAmountThawing": "12",
                    "sender": {
                        "id": "0x192c3B6e0184Fa0Cc5B9D2bDDEb6B79Fb216a002",
                        "authorizedSigners": []
                    }
                }
            ]
        }
    }
"#;

lazy_static! {
    pub static ref NETWORK_SUBGRAPH_DEPLOYMENT: DeploymentId = DeploymentId::from_str("QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP").unwrap();
    pub static ref ESCROW_SUBGRAPH_DEPLOYMENT: DeploymentId = DeploymentId::from_str("Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss").unwrap();

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
                    id: DeploymentId::from_str(
                        "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                    ).unwrap(),
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
                    id: DeploymentId::from_str(
                        "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf"
                    ).unwrap(),
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
                    id: DeploymentId::from_str(
                        "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                    ).unwrap(),
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
                    id: DeploymentId::from_str(
                        "0xc064c354bc21dd958b1d41b67b8ef161b75d2246b425f68ed4c74964ae705cbd"
                    ).unwrap(),
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

    pub static ref ESCROW_ACCOUNTS_BALANCES: HashMap<Address, U256> = HashMap::from([
        (Address::from_str("0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap(), U256::from(24)), // TAP_SENDER
        (Address::from_str("0x22d491bde2303f2f43325b2108d26f1eaba1e32b").unwrap(), U256::from(42)),
        (Address::from_str("0x192c3B6e0184Fa0Cc5B9D2bDDEb6B79Fb216a002").unwrap(), U256::from(2975)),
    ]);

    /// Maps signers back to their senders
    pub static ref ESCROW_ACCOUNTS_SIGNERS_TO_SENDERS: HashMap<Address, Address> = HashMap::from([
        (
            Address::from_str("0x533661F0fb14d2E8B26223C86a610Dd7D2260892").unwrap(), // TAP_SIGNER
            Address::from_str("0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap(), // TAP_SENDER
        ),
        (
            Address::from_str("0x2740f6fA9188cF53ffB6729DDD21575721dE92ce").unwrap(),
            Address::from_str("0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap(), // TAP_SENDER
        ),
        (
            Address::from_str("0x245059163ff6ee14279aa7b35ea8f0fdb967df6e").unwrap(),
            Address::from_str("0x22d491bde2303f2f43325b2108d26f1eaba1e32b").unwrap(),
        ),
    ]);

    pub static ref ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS: HashMap<Address, Vec<Address>> = HashMap::from([
        (
            Address::from_str("0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap(), // TAP_SENDER
            vec![
                Address::from_str("0x533661F0fb14d2E8B26223C86a610Dd7D2260892").unwrap(), // TAP_SIGNER
                Address::from_str("0x2740f6fA9188cF53ffB6729DDD21575721dE92ce").unwrap(),
            ],
        ),
        (
            Address::from_str("0x22d491bde2303f2f43325b2108d26f1eaba1e32b").unwrap(),
            vec![Address::from_str("0x245059163ff6ee14279aa7b35ea8f0fdb967df6e").unwrap()],
        ),
        (
            Address::from_str("0x192c3B6e0184Fa0Cc5B9D2bDDEb6B79Fb216a002").unwrap(),
            vec![],
        ),
    ]);

    /// Fixture to generate a wallet and address.
    /// Address: 0x9858EfFD232B4033E47d90003D41EC34EcaEda94
    pub static ref TAP_SENDER: (LocalWallet, Address) = {
        let wallet: LocalWallet = MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, Address::from_slice(address.as_bytes()))
    };

    /// Fixture to generate a wallet and address.
    /// Address: 0x533661F0fb14d2E8B26223C86a610Dd7D2260892
    pub static ref TAP_SIGNER: (LocalWallet, Address) = {
        let wallet: LocalWallet = MnemonicBuilder::<English>::default()
            .phrase("rude pipe parade travel organ vendor card festival magnet novel forget refuse keep draft tool")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, Address::from_slice(address.as_bytes()))
    };

    pub static ref TAP_EIP712_DOMAIN: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address::from([0x11u8; 20]),
    };
}

/// Function to generate a signed receipt using the TAP_SIGNER wallet.
pub async fn create_signed_receipt(
    allocation_id: Address,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
) -> SignedReceipt {
    let (wallet, _) = &*self::TAP_SIGNER;

    EIP712SignedMessage::new(
        &self::TAP_EIP712_DOMAIN,
        Receipt {
            allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        wallet,
    )
    .await
    .unwrap()
}
