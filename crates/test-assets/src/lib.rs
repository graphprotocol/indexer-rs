// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bip39::Mnemonic;
use indexer_allocation::{Allocation, AllocationStatus, NetworkAddress, SubgraphDeployment};
use lazy_static::lazy_static;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::{Receipt, SignedReceipt};
use thegraph_core::{
    alloy::{
        primitives::{address, Address, U256},
        signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
        sol_types::Eip712Domain,
    },
    deployment_id, DeploymentId,
};
use tokio::sync::Notify;
use typed_builder::TypedBuilder;

/// Assert something is true while sleeping and retrying
///
/// This macro creates a loop that keeps retrying the expression
/// by default every 50 milliseconds.
/// In case, the assertion is not true after the timeout period
/// (default to 1 second), this macro panics
#[macro_export]
macro_rules! assert_while_retry {
    ($assertion:expr) => {
        assert_while_retry!(
            $assertion,
            "Assertion was not true while retrying every 50 milliseconds up to 1 second.",
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(50)
        );
    };
    ($assertion:expr, $msg:expr, $timeout:expr, $sleep:expr) => {
        if tokio::time::timeout($timeout, async {
            loop {
                if $assertion {
                    tokio::time::sleep($sleep).await;
                } else {
                    break;
                }
            }
        })
        .await
        .is_err()
        {
            panic!($msg);
        }
    };
}

// The allocation IDs below are generated using the mnemonic
// "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
// and the following epoch and index:
//
// - (createdAtEpoch, 0)
// - (createdAtEpoch-1, 0)
// - (createdAtEpoch, 2)
// - (createdAtEpoch-1, 1)
//
// Using https://github.com/graphprotocol/indexer/blob/f8786c979a8ed0fae93202e499f5ce25773af473/packages/indexer-common/src/allocations/keys.ts#L41-L71
pub const ESCROW_QUERY_RESPONSE: &str = r#"
    {
        "data": {
            "escrowAccounts": [
                {
                    "balance": "34",
                    "totalAmountThawing": "10",
                    "sender": {
                        "id": "0x9858EfFD232B4033E47d90003D41EC34EcaEda94",
                        "signers": [
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
                        "signers": [
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
                        "signers": []
                    }
                }
            ]
        }
    }
"#;

pub const NETWORK_SUBGRAPH_DEPLOYMENT: DeploymentId =
    deployment_id!("QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP");

pub const ESCROW_SUBGRAPH_DEPLOYMENT: DeploymentId =
    deployment_id!("Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss");

pub const INDEXER_ADDRESS: Address = address!("d75c4dbcb215a6cf9097cfbcc70aab2596b96a9c");

pub const DISPUTE_MANAGER_ADDRESS: Address = address!("deadbeefcafebabedeadbeefcafebabedeadbeef");

pub const ALLOCATION_ID_0: Address = address!("fa44c72b753a66591f241c7dc04e8178c30e13af");

pub const ALLOCATION_ID_1: Address = address!("dd975e30aafebb143e54d215db8a3e8fd916a701");

pub const ALLOCATION_ID_2: Address = address!("a171cd12c3dde7eb8fe7717a0bcd06f3ffa65658");

pub const ALLOCATION_ID_3: Address = address!("69f961358846fdb64b04e1fd7b2701237c13cd9a");

pub const VERIFIER_ADDRESS: Address = address!("1111111111111111111111111111111111111111");

lazy_static! {
    pub static ref INDEXER_MNEMONIC: Mnemonic = Mnemonic::from_str(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    ).unwrap();

    /// These are the expected json-serialized contents of the value returned by
    /// AllocationMonitor::current_eligible_allocations with the values above at epoch threshold 940.
    pub static ref INDEXER_ALLOCATIONS: HashMap<Address, Allocation> = HashMap::from([
        (
            ALLOCATION_ID_0,
            Allocation {
                id: ALLOCATION_ID_0,
                indexer: address!("d75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"),
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
            ALLOCATION_ID_1,
            Allocation {
                id: ALLOCATION_ID_1,
                indexer: address!("d75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"),
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
            ALLOCATION_ID_2,
            Allocation {
                id: ALLOCATION_ID_2,
                indexer: address!("d75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"),
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
            ALLOCATION_ID_3,
            Allocation {
                id: ALLOCATION_ID_3,
                indexer: address!("d75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"),
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

    pub static ref ESCROW_ACCOUNTS_BALANCES: HashMap<NetworkAddress, U256> = HashMap::from([
        (NetworkAddress::Legacy(address!("9858EfFD232B4033E47d90003D41EC34EcaEda94")), U256::from(24)), // TAP_SENDER
        (NetworkAddress::Legacy(address!("22d491bde2303f2f43325b2108d26f1eaba1e32b")), U256::from(42)),
        (NetworkAddress::Legacy(address!("192c3B6e0184Fa0Cc5B9D2bDDEb6B79Fb216a002")), U256::from(2975)),
    ]);


    /// Maps signers back to their senders
    pub static ref ESCROW_ACCOUNTS_SIGNERS_TO_SENDERS: HashMap<NetworkAddress, NetworkAddress> = HashMap::from([
        (
            NetworkAddress::Legacy(address!("533661F0fb14d2E8B26223C86a610Dd7D2260892")), // TAP_SIGNER
            NetworkAddress::Legacy(address!("9858EfFD232B4033E47d90003D41EC34EcaEda94")), // TAP_SENDER
        ),
        (
            NetworkAddress::Legacy(address!("2740f6fA9188cF53ffB6729DDD21575721dE92ce")),
            NetworkAddress::Legacy(address!("9858EfFD232B4033E47d90003D41EC34EcaEda94")), // TAP_SENDER
        ),
        (
            NetworkAddress::Legacy(address!("245059163ff6ee14279aa7b35ea8f0fdb967df6e")),
            NetworkAddress::Legacy(address!("22d491bde2303f2f43325b2108d26f1eaba1e32b")),
        ),
    ]);


    pub static ref ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS: HashMap<NetworkAddress, Vec<Address>> = HashMap::from([
        (
            NetworkAddress::Legacy(address!("9858EfFD232B4033E47d90003D41EC34EcaEda94")), // TAP_SENDER
            vec![
                address!("533661F0fb14d2E8B26223C86a610Dd7D2260892"), // TAP_SIGNER
                address!("2740f6fA9188cF53ffB6729DDD21575721dE92ce"),
            ],
        ),
        (
            NetworkAddress::Legacy(address!("22d491bde2303f2f43325b2108d26f1eaba1e32b")),
            vec![address!("245059163ff6ee14279aa7b35ea8f0fdb967df6e")],
        ),
        (
            NetworkAddress::Legacy(address!("192c3B6e0184Fa0Cc5B9D2bDDEb6B79Fb216a002")),
            vec![],
        ),
    ]);


    /// Fixture to generate a wallet and address.
    /// Address: 0x9858EfFD232B4033E47d90003D41EC34EcaEda94
    pub static ref TAP_SENDER: (PrivateKeySigner, Address) = {
        let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, address)
    };

    /// Fixture to generate a wallet and address.
    /// Address: 0x533661F0fb14d2E8B26223C86a610Dd7D2260892
    pub static ref TAP_SIGNER: (PrivateKeySigner, Address) = {
        let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
            .phrase("rude pipe parade travel organ vendor card festival magnet novel forget refuse keep draft tool")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, address)
    };

    pub static ref TAP_EIP712_DOMAIN: Eip712Domain = tap_eip712_domain(
        1,
        VERIFIER_ADDRESS
    );
}

#[derive(TypedBuilder)]
pub struct SignedReceiptRequest {
    #[builder(default = Address::ZERO)]
    allocation_id: Address,
    #[builder(default)]
    nonce: u64,
    #[builder(default_code = r#"SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64"#)]
    timestamp_ns: u64,
    #[builder(default = 1)]
    value: u128,
}

/// Function to generate a signed receipt using the TAP_SIGNER wallet.
pub async fn create_signed_receipt(
    SignedReceiptRequest {
        allocation_id,
        nonce,
        timestamp_ns,
        value,
    }: SignedReceiptRequest,
) -> SignedReceipt {
    let (wallet, _) = &*self::TAP_SIGNER;

    Eip712SignedMessage::new(
        &self::TAP_EIP712_DOMAIN,
        Receipt {
            allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        wallet,
    )
    .unwrap()
}

pub async fn flush_messages(notify: &Notify) {
    loop {
        if tokio::time::timeout(Duration::from_millis(10), notify.notified())
            .await
            .is_err()
        {
            break;
        }
    }
}
