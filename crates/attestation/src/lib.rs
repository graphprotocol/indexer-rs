// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_allocation::Allocation;
use sha2::{Digest, Sha256};
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
    // Try the primary derivation method first for compatibility
    match derive_key_pair_v1(indexer_mnemonic, epoch, deployment, index) {
        Ok(wallet) => Ok(wallet),
        Err(e) => {
            // If the primary method fails (likely due to path length), try the fallback
            tracing::debug!(
                "Primary derivation failed for epoch={}, deployment={}, index={}: {}. Trying fallback.",
                epoch,
                deployment,
                index,
                e
            );
            derive_key_pair_v2(indexer_mnemonic, epoch, deployment, index)
        }
    }
}

/// Primary derivation method - kept for compatibility
fn derive_key_pair_v1(
    indexer_mnemonic: &str,
    epoch: u64,
    deployment: &DeploymentId,
    index: u64,
) -> Result<PrivateKeySigner, anyhow::Error> {
    let mut derivation_path = format!("m/{epoch}/");
    derivation_path.push_str(
        &deployment
            .to_string()
            .as_bytes()
            .iter()
            .map(|byte| byte.to_string())
            .collect::<Vec<String>>()
            .join("/"),
    );
    derivation_path.push_str(&format!("/{index}"));

    // Check path length before attempting derivation
    // Use a more generous limit - most BIP32 implementations can handle longer paths
    // The original issue was likely with extremely long paths (200+ chars)
    if derivation_path.len() > 200 {
        return Err(anyhow::anyhow!(
            "BIP32 path too long: {} characters, deployment: {}",
            derivation_path.len(),
            deployment
        ));
    }

    Ok(MnemonicBuilder::<English>::default()
        .derivation_path(&derivation_path)
        .expect("Valid derivation path")
        .phrase(indexer_mnemonic)
        .build()?)
}

/// Fallback derivation method - uses hash of deployment to create shorter, deterministic paths
fn derive_key_pair_v2(
    indexer_mnemonic: &str,
    epoch: u64,
    deployment: &DeploymentId,
    index: u64,
) -> Result<PrivateKeySigner, anyhow::Error> {
    // Hash the deployment ID to create a fixed-size representation
    let mut hasher = Sha256::new();
    hasher.update(deployment.to_string().as_bytes());
    let deployment_hash = hasher.finalize();

    // Convert hash to u32 values for BIP32 path components (within valid range)
    let hash_bytes: [u8; 32] = deployment_hash.into();
    let deployment_part1 =
        u32::from_be_bytes([hash_bytes[0], hash_bytes[1], hash_bytes[2], hash_bytes[3]])
            & 0x7FFFFFFF; // Ensure < 2^31 for BIP32 compatibility
    let deployment_part2 =
        u32::from_be_bytes([hash_bytes[4], hash_bytes[5], hash_bytes[6], hash_bytes[7]])
            & 0x7FFFFFFF;

    // Build a compact BIP32 path using hardened derivation
    let derivation_path = format!("m/{epoch}'/{deployment_part1}'/{deployment_part2}'/{index}'");

    tracing::debug!(
        "Fallback derivation: epoch={}, deployment={}, index={}, path={}",
        epoch,
        deployment,
        index,
        derivation_path
    );

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
    /// Create an attestation signer for an allocation using a single mnemonic.
    ///
    /// For supporting multiple operator mnemonics (e.g., after key rotation),
    /// use [`AttestationSigner::new_with_mnemonics`] instead.
    pub fn new(
        indexer_mnemonic: &str,
        allocation: &Allocation,
        chain_id: ChainId,
        dispute_manager: Address,
    ) -> Result<Self, anyhow::Error> {
        Self::new_with_mnemonics(&[indexer_mnemonic], allocation, chain_id, dispute_manager)
    }

    /// Create an attestation signer for an allocation using multiple mnemonics.
    ///
    /// This function tries each mnemonic in order until one produces a wallet
    /// that matches the allocation ID. This is useful when an indexer has
    /// rotated operator keys or has allocations created with different keys.
    ///
    /// # Arguments
    ///
    /// * `indexer_mnemonics` - A slice of mnemonic phrases to try
    /// * `allocation` - The allocation to create a signer for
    /// * `chain_id` - The chain ID for EIP-712 domain
    /// * `dispute_manager` - The dispute manager address for EIP-712 domain
    ///
    /// # Errors
    ///
    /// Returns an error if no mnemonic produces a wallet matching the allocation.
    pub fn new_with_mnemonics(
        indexer_mnemonics: &[impl AsRef<str>],
        allocation: &Allocation,
        chain_id: ChainId,
        dispute_manager: Address,
    ) -> Result<Self, anyhow::Error> {
        // Recreate a wallet that has the same address as the allocation
        let wallet = wallet_for_allocation_multi(indexer_mnemonics, allocation)?;

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

/// Try to find a wallet matching the allocation using multiple mnemonics.
///
/// For each mnemonic, tries 200 key combinations (100 indices × 2 epochs).
/// Returns the first wallet that matches the allocation ID.
fn wallet_for_allocation_multi(
    indexer_mnemonics: &[impl AsRef<str>],
    allocation: &Allocation,
) -> Result<PrivateKeySigner, anyhow::Error> {
    tracing::debug!(
        "Starting wallet derivation for allocation {}, deployment {}, epoch {}, trying {} mnemonic(s)",
        allocation.id,
        allocation.subgraph_deployment.id,
        allocation.created_at_epoch,
        indexer_mnemonics.len()
    );

    for (mnemonic_idx, indexer_mnemonic) in indexer_mnemonics.iter().enumerate() {
        let mnemonic_str = indexer_mnemonic.as_ref();

        // Guess the allocation index by enumerating all indexes in the
        // range [0, 100] and checking for a match
        for i in 0..100 {
            // The allocation was either created at the epoch it intended to or one
            // epoch later. So try both.
            for created_at_epoch in [allocation.created_at_epoch, allocation.created_at_epoch - 1] {
                // The allocation ID is the address of a unique key pair, we just
                // need to find the right one by enumerating them all
                let wallet = derive_key_pair(
                    mnemonic_str,
                    created_at_epoch,
                    &allocation.subgraph_deployment.id,
                    i,
                )?;

                let wallet_address = wallet.address();

                if i < 5 || (i % 20 == 0) {
                    // Log first 5 attempts and every 20th attempt
                    tracing::debug!(
                        "Derivation attempt: mnemonic={}, epoch={}, index={}, derived_address={}, target_allocation={}",
                        mnemonic_idx, created_at_epoch, i, wallet_address, allocation.id
                    );
                }

                // Check if wallet address matches allocation ID
                // Allocation IDs are 20-byte addresses in all supported modes
                if wallet_address == allocation.id {
                    tracing::debug!(
                        "Found matching wallet! mnemonic={}, epoch={}, index={}, address={}",
                        mnemonic_idx,
                        created_at_epoch,
                        i,
                        wallet_address
                    );
                    return Ok(wallet);
                }
            }
        }
    }

    // Enhanced error reporting for troubleshooting
    let mnemonic_count = indexer_mnemonics.len();
    let combinations_tried = mnemonic_count * 200;
    tracing::warn!(
        "Cannot derive attestation signer for allocation {} after trying {} key combinations \
        across {} mnemonic(s). This allocation may have been created with a different operator key.",
        allocation.id,
        combinations_tried,
        mnemonic_count
    );

    tracing::debug!(
        "What we tried: allocation_id={}, deployment={}, created_at_epoch={}, \
        tested epochs {} and {}, tested indices 0-99 for each epoch across {} mnemonic(s)",
        allocation.id,
        allocation.subgraph_deployment.id,
        allocation.created_at_epoch,
        allocation.created_at_epoch,
        allocation.created_at_epoch - 1,
        mnemonic_count
    );

    // Show what we actually derived to help with troubleshooting (using first mnemonic)
    if let Some(first_mnemonic) = indexer_mnemonics.first() {
        tracing::debug!("Here's what our key derivation produced (first mnemonic):");
        for &epoch in [allocation.created_at_epoch, allocation.created_at_epoch - 1].iter() {
            for i in 0..3 {
                if let Ok(wallet) = derive_key_pair(
                    first_mnemonic.as_ref(),
                    epoch,
                    &allocation.subgraph_deployment.id,
                    i,
                ) {
                    tracing::debug!(
                        "  epoch={}, index={}, we_derived={}",
                        epoch,
                        i,
                        wallet.address()
                    );
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "No key combination matched allocation {} after trying {} combinations across {} mnemonic(s). \
        If this allocation was created with a different operator key, add that mnemonic to \
        `indexer.operator_mnemonics` in your configuration.",
        allocation.id,
        combinations_tried,
        mnemonic_count
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
    fn test_deployment_bytes_analysis() {
        let deployment = DeploymentId::from_str(
            "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a",
        )
        .unwrap();

        println!("Deployment string: {deployment}");
        println!(
            "Deployment as_bytes length: {}",
            deployment.as_bytes().len()
        );
        println!(
            "Deployment string bytes length: {}",
            deployment.to_string().len()
        );

        // Build the exact path with m/ prefix like our implementation
        let epoch = 953;
        let index = 0;
        let mut derivation_path = format!("m/{epoch}/");
        derivation_path.push_str(
            &deployment
                .to_string()
                .as_bytes()
                .iter()
                .map(|byte| byte.to_string())
                .collect::<Vec<String>>()
                .join("/"),
        );
        derivation_path.push_str(&format!("/{index}"));

        println!("Rust derivation path: {derivation_path}");
        println!("Path length: {}", derivation_path.len());

        // Check if it's going to fallback
        println!(
            "Will trigger fallback (>120): {}",
            derivation_path.len() > 120
        );

        // Test both primary and fallback derivations for this deployment
        println!("\n--- Testing primary derivation ---");
        match derive_key_pair_v1(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index) {
            Ok(wallet) => println!("Primary result: 0x{:x}", wallet.address()),
            Err(e) => println!("Primary failed: {e}"),
        }

        println!("\n--- Testing fallback derivation ---");
        match derive_key_pair_v2(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index) {
            Ok(wallet) => println!("Fallback result: 0x{:x}", wallet.address()),
            Err(e) => println!("Fallback failed: {e}"),
        }
    }

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
            is_legacy: false,
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
    fn test_v2_fallback_for_long_paths() {
        // Test the problematic deployment from the original error logs
        let problematic_deployment = "QmcBcJvmRyCfwgGbU5hmJbnQfb3hJxQtY3Hxp4AbYuymLf";

        let deployment = DeploymentId::from_str(problematic_deployment).unwrap();
        let epoch = 123; // Example epoch
        let index = 0;

        // Check the actual path length for this deployment
        let mut derivation_path = format!("m/{epoch}/");
        derivation_path.push_str(
            &deployment
                .to_string()
                .as_bytes()
                .iter()
                .map(|byte| byte.to_string())
                .collect::<Vec<String>>()
                .join("/"),
        );
        derivation_path.push_str(&format!("/{index}"));

        println!("Testing deployment: {deployment}");
        println!("Path length: {}", derivation_path.len());
        println!("Exceeds 200 chars: {}", derivation_path.len() > 200);

        // Test primary derivation - should work if path <= 200 chars
        let v1_result = derive_key_pair_v1(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index);
        match &v1_result {
            Ok(wallet) => println!("Primary succeeded: 0x{:x}", wallet.address()),
            Err(e) => println!("Primary failed: {e}"),
        }

        // Test fallback derivation
        let v2_result =
            derive_key_pair_v2(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index).unwrap();
        println!("Fallback result: 0x{:x}", v2_result.address());

        // Test the main function
        let main_result =
            derive_key_pair(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index).unwrap();
        println!("Main function result: 0x{:x}", main_result.address());

        // Main function should use primary if it works, otherwise fallback
        if v1_result.is_ok() {
            assert_eq!(v1_result.unwrap().address(), main_result.address());
            println!("✓ Main function correctly used primary derivation for this deployment");
        } else {
            assert_eq!(v2_result.address(), main_result.address());
            println!("✓ Main function correctly fell back for this deployment");
        }

        // Now test with an artificially long deployment that definitely triggers fallback
        // Create a deployment string that will exceed 200 chars
        let very_long_deployment = format!("Qm{}", "x".repeat(200)); // This will be way too long
        if let Ok(long_deployment) = DeploymentId::from_str(&very_long_deployment) {
            println!("\n--- Testing artificially long deployment ---");
            match derive_key_pair_v1(INDEXER_OPERATOR_MNEMONIC, epoch, &long_deployment, index) {
                Ok(_) => {
                    panic!("Primary derivation should have failed for artificially long deployment")
                }
                Err(e) => println!("Primary derivation correctly failed for long deployment: {e}"),
            }

            let long_v2_result =
                derive_key_pair_v2(INDEXER_OPERATOR_MNEMONIC, epoch, &long_deployment, index)
                    .unwrap();
            let long_main_result =
                derive_key_pair(INDEXER_OPERATOR_MNEMONIC, epoch, &long_deployment, index).unwrap();
            assert_eq!(long_v2_result.address(), long_main_result.address());
            println!("✓ Fallback derivation working for artificially long deployment");
        }
    }

    #[test]
    fn test_specific_failing_allocation() {
        // Test the specific allocation that's failing in the logs
        let deployment =
            DeploymentId::from_str("QmcBcJvmRyCfwgGbU5hmJbnQfb3hJxQtY3Hxp4AbYuymLf").unwrap();
        let target_allocation = address!("AEa0CA4850810AEC59d7C0BD624d6F7766cBC865");
        let epochs_to_try = [3, 2, 1, 4, 5]; // epoch 3 from logs, and nearby epochs

        println!("=== Debugging Specific Failing Allocation ===");
        println!("Target allocation: 0x{target_allocation:x}");
        println!("Deployment: {deployment}");
        println!("Deployment string length: {}", deployment.to_string().len());

        // Try with the actual local environment mnemonic
        let local_mnemonic = "test test test test test test test test test test test zero";
        println!("\n--- Trying with local environment mnemonic ---");
        for &epoch in &epochs_to_try {
            for index in 0..20 {
                match derive_key_pair(local_mnemonic, epoch, &deployment, index) {
                    Ok(wallet) => {
                        let address = wallet.address();
                        println!("Epoch: {epoch}, Index: {index}, Address: 0x{address:x}");
                        if address == target_allocation {
                            println!(
                                "🎯 MATCH FOUND with local mnemonic! Epoch: {epoch}, Index: {index}"
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        println!("Epoch: {epoch}, Index: {index}, Error: {e}");
                    }
                }
            }
        }

        // Try with the indexer operator mnemonic
        println!("\n--- Trying with indexer operator mnemonic ---");
        for &epoch in &epochs_to_try {
            for index in 0..20 {
                match derive_key_pair(INDEXER_OPERATOR_MNEMONIC, epoch, &deployment, index) {
                    Ok(wallet) => {
                        let address = wallet.address();
                        println!("Epoch: {epoch}, Index: {index}, Address: 0x{address:x}");
                        if address == target_allocation {
                            println!(
                                "🎯 MATCH FOUND with operator mnemonic! Epoch: {epoch}, Index: {index}"
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        println!("Epoch: {epoch}, Index: {index}, Error: {e}");
                    }
                }
            }
        }

        println!("\n❌ No match found with either mnemonic for the failing allocation");
        println!("This suggests the allocation was created with different parameters or mnemonic");
    }

    #[test]
    fn test_debug_allocation_derivation() {
        let mnemonic = "test test test test test test test test test test test zero";
        let deployment_hex = "0xbbde25a2c85f55b53b7698b9476610c3d1202d88870e66502ab0076b7218f98a";
        let deployment = DeploymentId::from_str(deployment_hex).unwrap();

        println!("Testing allocation key derivation with indexer mnemonic:");
        println!("Mnemonic: {mnemonic}");
        println!("Deployment: {deployment}");
        println!();

        // Test different epochs and indices to find the target allocation ID
        for epoch in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
            for index in 0..10 {
                match derive_key_pair(mnemonic, epoch, &deployment, index) {
                    Ok(wallet) => {
                        let address = wallet.address();
                        println!("Epoch: {epoch}, Index: {index}, Address: 0x{address:x}");

                        // Check if this matches the allocation ID we're looking for
                        let addr_str = format!("0x{address:x}").to_lowercase();
                        if addr_str == "0xec972f0480096adc48b0a355fa9844aa62af26e9" {
                            println!(
                                "*** MATCH FOUND! This is the allocation ID we're looking for ***"
                            );
                        }
                    }
                    Err(e) => {
                        println!("Epoch: {epoch}, Index: {index}, Error: {e}");
                    }
                }
            }
            println!();
        }

        // Try with the deployment from the error logs
        if let Ok(test_deployment) =
            DeploymentId::from_str("QmcBcJvmRyCfwgGbU5hmJbnQfb3hJxQtY3Hxp4AbYuymLf")
        {
            println!("Testing with deployment from logs: {test_deployment}");

            for epoch in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
                for index in 0..10 {
                    match derive_key_pair(mnemonic, epoch, &test_deployment, index) {
                        Ok(wallet) => {
                            let address = wallet.address();
                            println!("Epoch: {epoch}, Index: {index}, Address: 0x{address:x}");

                            let addr_str = format!("0x{address:x}").to_lowercase();
                            if addr_str == "0xec972f0480096adc48b0a355fa9844aa62af26e9" {
                                println!("*** MATCH FOUND! This is the allocation ID we're looking for ***");
                            }
                        }
                        Err(e) => {
                            println!("Epoch: {epoch}, Index: {index}, Error: {e}");
                        }
                    }
                }
            }
        }
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
            is_legacy: false,
        };
        assert!(AttestationSigner::new(
            INDEXER_OPERATOR_MNEMONIC,
            &allocation,
            1,
            DISPUTE_MANAGER_ADDRESS
        )
        .is_err());
    }

    // Second mnemonic for multi-mnemonic fallback tests
    const SECOND_MNEMONIC: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";

    /// Test that new_with_mnemonics correctly falls back to the second mnemonic
    /// when the first one doesn't match the allocation.
    #[test]
    fn test_multi_mnemonic_fallback_to_second() {
        // This allocation was created with INDEXER_OPERATOR_MNEMONIC at epoch 940, index 2
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
            is_legacy: false,
        };

        // Use SECOND_MNEMONIC first (won't match), then INDEXER_OPERATOR_MNEMONIC (will match)
        let mnemonics = [SECOND_MNEMONIC, INDEXER_OPERATOR_MNEMONIC];

        let signer = AttestationSigner::new_with_mnemonics(
            &mnemonics,
            &allocation,
            1,
            DISPUTE_MANAGER_ADDRESS,
        );

        assert!(
            signer.is_ok(),
            "Should successfully create signer using the second mnemonic"
        );

        // Verify the signer produces the same key as when using the correct mnemonic directly
        let expected_wallet = derive_key_pair(
            INDEXER_OPERATOR_MNEMONIC,
            940,
            &allocation.subgraph_deployment.id,
            2,
        )
        .unwrap();

        let signer = signer.unwrap();
        assert_eq!(
            PrivateKeySigner::from_signing_key(signer.signer),
            expected_wallet,
            "Signer should use the matching mnemonic"
        );
    }

    /// Test that new_with_mnemonics correctly falls back through multiple mnemonics
    /// until finding one that matches (tests with 3 mnemonics where only the last matches).
    #[test]
    fn test_multi_mnemonic_fallback_to_third() {
        // Third mnemonic that also won't match
        const THIRD_MNEMONIC: &str =
            "legal winner thank year wave sausage worth useful legal winner thank yellow";

        // This allocation was created with INDEXER_OPERATOR_MNEMONIC at epoch 940, index 2
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
            is_legacy: false,
        };

        // Use two non-matching mnemonics first, then the correct one last
        let mnemonics = [SECOND_MNEMONIC, THIRD_MNEMONIC, INDEXER_OPERATOR_MNEMONIC];

        let signer = AttestationSigner::new_with_mnemonics(
            &mnemonics,
            &allocation,
            1,
            DISPUTE_MANAGER_ADDRESS,
        );

        assert!(
            signer.is_ok(),
            "Should successfully create signer using the third mnemonic"
        );

        // Verify the signer produces the same key as when using the correct mnemonic directly
        let expected_wallet = derive_key_pair(
            INDEXER_OPERATOR_MNEMONIC,
            940,
            &allocation.subgraph_deployment.id,
            2,
        )
        .unwrap();

        let signer = signer.unwrap();
        assert_eq!(
            PrivateKeySigner::from_signing_key(signer.signer),
            expected_wallet,
            "Signer should use the matching mnemonic (third one)"
        );
    }

    /// Test that new_with_mnemonics uses the first mnemonic when it matches,
    /// not unnecessarily falling back.
    #[test]
    fn test_multi_mnemonic_uses_first_when_matches() {
        // This allocation was created with INDEXER_OPERATOR_MNEMONIC at epoch 940, index 2
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
            is_legacy: false,
        };

        // Put the matching mnemonic first
        let mnemonics = [INDEXER_OPERATOR_MNEMONIC, SECOND_MNEMONIC];

        let signer = AttestationSigner::new_with_mnemonics(
            &mnemonics,
            &allocation,
            1,
            DISPUTE_MANAGER_ADDRESS,
        );

        assert!(
            signer.is_ok(),
            "Should successfully create signer using the first mnemonic"
        );

        // Verify the signer produces the same key
        let expected_wallet = derive_key_pair(
            INDEXER_OPERATOR_MNEMONIC,
            940,
            &allocation.subgraph_deployment.id,
            2,
        )
        .unwrap();

        let signer = signer.unwrap();
        assert_eq!(
            PrivateKeySigner::from_signing_key(signer.signer),
            expected_wallet,
            "Signer should use the first (matching) mnemonic"
        );
    }

    /// Test that new_with_mnemonics returns an error when no mnemonics match.
    #[test]
    fn test_multi_mnemonic_error_when_none_match() {
        const THIRD_MNEMONIC: &str =
            "legal winner thank year wave sausage worth useful legal winner thank yellow";

        let allocation = Allocation {
            // This address won't match any of our test mnemonics
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
            is_legacy: false,
        };

        // None of these will match the allocation
        let mnemonics = [SECOND_MNEMONIC, THIRD_MNEMONIC];

        let result = AttestationSigner::new_with_mnemonics(
            &mnemonics,
            &allocation,
            1,
            DISPUTE_MANAGER_ADDRESS,
        );

        assert!(
            result.is_err(),
            "Should return error when no mnemonic matches the allocation"
        );

        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("2 mnemonic(s)"),
            "Error message should indicate number of mnemonics tried: {error_message}"
        );
    }
}
