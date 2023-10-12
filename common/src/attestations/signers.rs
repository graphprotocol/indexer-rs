// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use ethers_core::types::U256;
use eventuals::{join, Eventual, EventualExt};
use log::warn;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::prelude::{Allocation, AttestationSigner};

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn attestation_signers(
    indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    indexer_mnemonic: String,
    chain_id: U256,
    dispute_manager: Eventual<Address>,
) -> Eventual<HashMap<Address, AttestationSigner>> {
    let attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>> =
        Box::leak(Box::new(Mutex::new(HashMap::new())));

    let indexer_mnemonic = Arc::new(indexer_mnemonic);

    // Whenever the indexer's active or recently closed allocations change, make sure
    // we have attestation signers for all of them
    join((indexer_allocations, dispute_manager)).map(move |(allocations, dispute_manager)| {
        let indexer_mnemonic = indexer_mnemonic.clone();
        async move {
            let mut signers = attestation_signers_map.lock().await;

            // Remove signers for allocations that are no longer active or recently closed
            signers.retain(|id, _| allocations.contains_key(id));

            // Create signers for new allocations
            for (id, allocation) in allocations.iter() {
                if !signers.contains_key(id) {
                    let signer = AttestationSigner::new(
                        &indexer_mnemonic,
                        allocation,
                        chain_id,
                        dispute_manager,
                    );
                    if let Err(e) = signer {
                        warn!(
                            "Failed to establish signer for allocation {}, deployment {}, createdAtEpoch {}: {}",
                            allocation.id, allocation.subgraph_deployment.id,
                            allocation.created_at_epoch, e
                        );
                    } else {
                        signers.insert(*id, signer.unwrap());
                    }
                }
            }

            signers.clone()
        }
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;

    use crate::test_vectors::{
        DISPUTE_MANAGER_ADDRESS, INDEXER_ALLOCATIONS, INDEXER_OPERATOR_MNEMONIC,
    };

    use super::*;

    #[tokio::test]
    async fn test_attestation_signers_update_with_allocations() {
        let (mut allocations_writer, allocations) = Eventual::<HashMap<Address, Allocation>>::new();
        let (mut dispute_manager_writer, dispute_manager) = Eventual::<Address>::new();

        dispute_manager_writer.write(*DISPUTE_MANAGER_ADDRESS);

        let signers = attestation_signers(
            allocations,
            (*INDEXER_OPERATOR_MNEMONIC).to_string(),
            U256::from(1),
            dispute_manager,
        );
        let mut signers = signers.subscribe();

        // Test that an empty set of allocations leads to an empty set of signers
        allocations_writer.write(HashMap::new());
        let latest_signers = signers.next().await.unwrap();
        assert_eq!(latest_signers, HashMap::new());

        // Test that writing our set of test allocations results in corresponding signers for all of them
        allocations_writer.write((*INDEXER_ALLOCATIONS).clone());
        let latest_signers = signers.next().await.unwrap();
        assert_eq!(latest_signers.len(), INDEXER_ALLOCATIONS.len());
        for signer_allocation_id in latest_signers.keys() {
            assert!(INDEXER_ALLOCATIONS
                .keys()
                .any(|allocation_id| signer_allocation_id == allocation_id));
        }
    }
}
