// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use anyhow::Result;
use ethers::signers::coins_bip39::English;
use ethers::signers::MnemonicBuilder;
use ethers::signers::Signer;
use ethers::signers::Wallet;
use ethers_core::k256::ecdsa::SigningKey;
use ethers_core::types::U256;
use serde::Deserialize;
use serde::Deserializer;

use crate::common::types::SubgraphDeploymentID;

#[derive(Debug, Eq, PartialEq)]
pub struct Allocation {
    pub id: Address,
    pub status: AllocationStatus,
    pub subgraph_deployment: SubgraphDeployment,
    pub indexer: Address,
    pub allocated_tokens: U256,
    pub created_at_epoch: u64,
    pub created_at_block_hash: String,
    pub closed_at_epoch: Option<u64>,
    pub closed_at_epoch_start_block_hash: Option<String>,
    pub previous_epoch_start_block_hash: Option<String>,
    pub poi: Option<String>,
    pub query_fee_rebates: Option<U256>,
    pub query_fees_collected: Option<U256>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum AllocationStatus {
    Null,
    Active,
    Closed,
    Finalized,
    Claimed,
}

#[derive(Debug, Eq, PartialEq, Deserialize)]
pub struct SubgraphDeployment {
    pub id: SubgraphDeploymentID,
    #[serde(rename = "deniedAt")]
    pub denied_at: Option<u64>,
    #[serde(rename = "stakedTokens")]
    pub staked_tokens: U256,
    #[serde(rename = "signalledTokens")]
    pub signalled_tokens: U256,
    #[serde(rename = "queryFeesAmount")]
    pub query_fees_amount: U256,
}

impl<'d> Deserialize<'d> for Allocation {
    fn deserialize<D>(deserializer: D) -> Result<Allocation, D::Error>
    where
        D: Deserializer<'d>,
    {
        #[derive(Deserialize)]
        struct InnerIndexer {
            id: Address,
        }

        #[derive(Deserialize)]
        #[allow(non_snake_case)]
        struct Outer {
            id: Address,
            subgraphDeployment: SubgraphDeployment,
            indexer: InnerIndexer,
            allocatedTokens: U256,
            createdAtBlockHash: String,
            createdAtEpoch: u64,
            closedAtEpoch: Option<u64>,
        }

        let outer = Outer::deserialize(deserializer)?;

        Ok(Allocation {
            id: outer.id,
            status: AllocationStatus::Null,
            subgraph_deployment: outer.subgraphDeployment,
            indexer: outer.indexer.id,
            allocated_tokens: outer.allocatedTokens,
            created_at_epoch: outer.createdAtEpoch,
            created_at_block_hash: outer.createdAtBlockHash,
            closed_at_epoch: outer.closedAtEpoch,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        })
    }
}

pub fn derive_key_pair(
    indexer_mnemonic: &str,
    epoch: u64,
    deployment: &SubgraphDeploymentID,
    index: u64,
) -> Result<Wallet<SigningKey>> {
    let mut derivation_path = format!("m/{}/", epoch);
    derivation_path.push_str(
        &deployment
            .ipfs_hash()
            .as_bytes()
            .iter()
            .map(|char| char.to_string())
            .collect::<Vec<String>>()
            .join("/"),
    );
    derivation_path.push_str(format!("/{}", index).as_str());

    Ok(MnemonicBuilder::<English>::default()
        .derivation_path(&derivation_path)
        .expect("Valid derivation path")
        .phrase(indexer_mnemonic)
        .build()?)
}

pub fn allocation_signer(indexer_mnemonic: &str, allocation: &Allocation) -> Result<SigningKey> {
    // Guess the allocation index by enumerating all indexes in the
    // range [0, 100] and checking for a match
    for i in 0..100 {
        // The allocation was either created at the epoch it intended to or one
        // epoch later. So try both both.
        for created_at_epoch in [allocation.created_at_epoch, allocation.created_at_epoch - 1] {
            let allocation_wallet = derive_key_pair(
                indexer_mnemonic,
                created_at_epoch,
                &allocation.subgraph_deployment.id,
                i,
            )?;
            if allocation_wallet.address().as_fixed_bytes() == allocation.id {
                return Ok(allocation_wallet.signer().clone());
            }
        }
    }
    Err(anyhow::anyhow!(
        "Could not find allocation signer for allocation {}",
        allocation.id
    ))
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use super::*;
    use crate::common::types::SubgraphDeploymentID;

    const INDEXER_OPERATOR_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_derive_key_pair() {
        assert_eq!(
            derive_key_pair(
                INDEXER_OPERATOR_MNEMONIC,
                953,
                &SubgraphDeploymentID::new(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                )
                .unwrap(),
                0
            )
            .unwrap()
            .address()
            .as_fixed_bytes(),
            Address::from_str("0xfa44c72b753a66591f241c7dc04e8178c30e13af").unwrap()
        );

        assert_eq!(
            derive_key_pair(
                INDEXER_OPERATOR_MNEMONIC,
                940,
                &SubgraphDeploymentID::new(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a"
                )
                .unwrap(),
                2
            )
            .unwrap()
            .address()
            .as_fixed_bytes(),
            Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap()
        );
    }

    #[test]
    fn test_allocation_signer() {
        // Note that we use `derive_key_pair` to derive the private key

        let allocation = Allocation {
            id: Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: SubgraphDeploymentID::new(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                )
                .unwrap(),
                denied_at: None,
                staked_tokens: U256::zero(),
                signalled_tokens: U256::zero(),
                query_fees_amount: U256::zero(),
            },
            indexer: Address::ZERO,
            allocated_tokens: U256::zero(),
            created_at_epoch: 940,
            created_at_block_hash: "".to_string(),
            closed_at_epoch: None,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        };
        assert_eq!(
            allocation_signer(INDEXER_OPERATOR_MNEMONIC, &allocation).unwrap(),
            *derive_key_pair(
                INDEXER_OPERATOR_MNEMONIC,
                940,
                &allocation.subgraph_deployment.id,
                2
            )
            .unwrap()
            .signer()
        );
    }

    #[test]
    fn test_allocation_signer_error() {
        // Note that because allocation will try 200 derivations paths, this is a slow test

        let allocation = Allocation {
            // Purposefully wrong address
            id: Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: SubgraphDeploymentID::new(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                )
                .unwrap(),
                denied_at: None,
                staked_tokens: U256::zero(),
                signalled_tokens: U256::zero(),
                query_fees_amount: U256::zero(),
            },
            indexer: Address::ZERO,
            allocated_tokens: U256::zero(),
            created_at_epoch: 940,
            created_at_block_hash: "".to_string(),
            closed_at_epoch: None,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        };
        assert!(allocation_signer(INDEXER_OPERATOR_MNEMONIC, &allocation).is_err());
    }
}
