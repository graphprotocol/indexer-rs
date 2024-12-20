// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_allocation::Allocation;
use thegraph_core::{
    alloy::{
        primitives::{Address, ChainId},
        signers::{
            k256,
            local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
        },
        sol_types::Eip712Domain,
    },
    attestation,
    attestation::Attestation,
    DeploymentId,
};

pub fn derive_key_pair(
    indexer_mnemonic: &str,
    epoch: u64,
    deployment: &DeploymentId,
    index: u64,
) -> Result<PrivateKeySigner, anyhow::Error> {
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
    signer: k256::ecdsa::SigningKey,
}

impl AttestationSigner {
    pub fn new(
        indexer_mnemonic: &str,
        allocation: &Allocation,
        chain_id: ChainId,
        dispute_manager: Address,
    ) -> Result<Self, anyhow::Error> {
        // Recreate a wallet that has the same address as the allocation
        let wallet = wallet_for_allocation(indexer_mnemonic, allocation)?;

        Ok(Self {
            deployment: allocation.subgraph_deployment.id,
            domain: attestation::eip712_domain(chain_id, dispute_manager),
            signer: wallet.into_credential(),
        })
    }

    pub fn create_attestation(&self, request: &str, response: &str) -> Attestation {
        let wallet = PrivateKeySigner::from_signing_key(self.signer.clone());
        attestation::create(&self.domain, &wallet, &self.deployment, request, response)
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
) -> Result<PrivateKeySigner, anyhow::Error> {
    // Guess the allocation index by enumerating all indexes in the
    // range [0, 100] and checking for a match
    for i in 0..100 {
        // The allocation was either created at the epoch it intended to or one
        // epoch later. So try both.
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
            if wallet.address() == allocation.id {
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
    use std::str::FromStr;

    use indexer_allocation::{Allocation, AllocationStatus, SubgraphDeployment};
    use test_assets::DISPUTE_MANAGER_ADDRESS;
    use test_log::test;
    use thegraph_core::{
        alloy::{
            primitives::{address, Address, U256},
            signers::local::PrivateKeySigner,
        },
        DeploymentId,
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
            .address(),
            address!("fa44c72b753a66591f241c7dc04e8178c30e13af")
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
            .address(),
            address!("a171cd12c3dde7eb8fe7717a0bcd06f3ffa65658")
        );
    }

    #[test]
    fn test_attestation_signer() {
        // Note that we use `derive_key_pair` to derive the private key

        let allocation = Allocation {
            id: address!("a171cd12c3dde7eb8fe7717a0bcd06f3ffa65658"),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: DeploymentId::from_str(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                )
                .unwrap(),
                denied_at: None,
            },
            indexer: Address::ZERO,
            allocated_tokens: U256::ZERO,
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
            PrivateKeySigner::from_signing_key(
                AttestationSigner::new(
                    INDEXER_OPERATOR_MNEMONIC,
                    &allocation,
                    1,
                    DISPUTE_MANAGER_ADDRESS
                )
                .unwrap()
                .signer
            ),
            derive_key_pair(
                INDEXER_OPERATOR_MNEMONIC,
                940,
                &allocation.subgraph_deployment.id,
                2
            )
            .unwrap()
        );
    }

    #[test]
    fn test_attestation_signer_error() {
        // Note that because allocation will try 200 derivations paths, this is a slow test

        let allocation = Allocation {
            // Purposefully wrong address
            id: address!("deadbeefcafebabedeadbeefcafebabedeadbeef"),
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: DeploymentId::from_str(
                    "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
                )
                .unwrap(),
                denied_at: None,
            },
            indexer: Address::ZERO,
            allocated_tokens: U256::ZERO,
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
            1,
            DISPUTE_MANAGER_ADDRESS
        )
        .is_err());
    }
}
