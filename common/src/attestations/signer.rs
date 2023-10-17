// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::{Address, U256};
use alloy_sol_types::Eip712Domain;
use ethers::signers::coins_bip39::English;
use ethers::signers::{MnemonicBuilder, Signer, Wallet};
use ethers_core::k256::ecdsa::SigningKey;
use toolshed::thegraph::attestation::{self, Attestation};
use toolshed::thegraph::DeploymentId;

use crate::prelude::Allocation;

pub fn derive_key_pair(
    indexer_mnemonic: &str,
    epoch: u64,
    deployment: &DeploymentId,
    index: u64,
) -> Result<Wallet<SigningKey>, anyhow::Error> {
    let mut derivation_path = format!("m/{}/", epoch);
    derivation_path.push_str(
        &deployment
            .to_string()
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

/// An attestation signer tied to a specific allocation via its signer key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationSigner {
    deployment: DeploymentId,
    domain: Eip712Domain,
    signer: SigningKey,
}

impl AttestationSigner {
    pub fn new(
        indexer_mnemonic: &str,
        allocation: &Allocation,
        chain_id: ethers_core::types::U256,
        dispute_manager: Address,
    ) -> Result<Self, anyhow::Error> {
        // Recreate a wallet that has the same address as the allocation
        let wallet = wallet_for_allocation(indexer_mnemonic, allocation)?;

        let mut chain_id_buf = [0_u8; 32];
        chain_id.to_big_endian(&mut chain_id_buf);
        let chain_id = U256::from_be_bytes(chain_id_buf);

        Ok(Self {
            deployment: allocation.subgraph_deployment.id,
            domain: attestation::eip712_domain(chain_id, dispute_manager),
            signer: wallet.signer().clone(),
        })
    }

    pub fn create_attestation(&self, request: &str, response: &str) -> Attestation {
        attestation::create(
            &self.domain,
            &self.signer,
            &self.deployment,
            request,
            response,
        )
    }

    pub fn verify(
        &self,
        attestation: &Attestation,
        request: &str,
        response: &str,
        expected_signer: &Address,
    ) -> Result<(), attestation::VerificationError> {
        attestation::verify(
            &self.domain,
            attestation,
            expected_signer,
            request,
            response,
        )
    }
}

fn wallet_for_allocation(
    indexer_mnemonic: &str,
    allocation: &Allocation,
) -> Result<Wallet<SigningKey>, anyhow::Error> {
    // Guess the allocation index by enumerating all indexes in the
    // range [0, 100] and checking for a match
    for i in 0..100 {
        // The allocation was either created at the epoch it intended to or one
        // epoch later. So try both both.
        for created_at_epoch in [allocation.created_at_epoch, allocation.created_at_epoch - 1] {
            // The allocation ID is the address of a unique key pair, we just
            // need to find the right one by enumerating them all
            let wallet = derive_key_pair(
                indexer_mnemonic,
                created_at_epoch,
                &allocation.subgraph_deployment.id,
                i,
            )?;

            // See if we have a match, i.e. a wallet whose address is identical to the allocation ID
            if wallet.address().as_fixed_bytes() == allocation.id {
                return Ok(wallet);
            }
        }
    }
    Err(anyhow::anyhow!(
        "Could not generate wallet matching allocation {}",
        allocation.id
    ))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use ethers_core::types::U256;
    use std::str::FromStr;
    use test_log::test;

    use crate::{
        prelude::{Allocation, AllocationStatus, SubgraphDeployment},
        test_vectors::DISPUTE_MANAGER_ADDRESS,
    };

    use super::*;

    const INDEXER_OPERATOR_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_derive_key_pair() {
        assert_eq!(
            derive_key_pair(
                INDEXER_OPERATOR_MNEMONIC,
                953,
                &DeploymentId::from_str(
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
                &DeploymentId::from_str(
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
    fn test_attestation_signer() {
        // Note that we use `derive_key_pair` to derive the private key

        let allocation = Allocation {
            id: Address::from_str("0xa171cd12c3dde7eb8fe7717a0bcd06f3ffa65658").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: DeploymentId::from_str(
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
            AttestationSigner::new(
                INDEXER_OPERATOR_MNEMONIC,
                &allocation,
                U256::from(1),
                *DISPUTE_MANAGER_ADDRESS
            )
            .unwrap()
            .signer,
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
    fn test_attestation_signer_error() {
        // Note that because allocation will try 200 derivations paths, this is a slow test

        let allocation = Allocation {
            // Purposefully wrong address
            id: Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: DeploymentId::from_str(
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
        assert!(AttestationSigner::new(
            INDEXER_OPERATOR_MNEMONIC,
            &allocation,
            U256::from(1),
            *DISPUTE_MANAGER_ADDRESS
        )
        .is_err());
    }
}
