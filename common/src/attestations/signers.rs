// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use thegraph_core::{Address, ChainId};
use tokio::sync::{watch::Receiver, watch, Mutex};
use tracing::warn;

use crate::prelude::{Allocation, AttestationSigner};

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn attestation_signers(
    mut indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    indexer_mnemonic: String,
    chain_id: ChainId,
    mut dispute_manager: Receiver<Address>,
) -> Receiver<HashMap<Address, AttestationSigner>> {
    let attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>> =
        Box::leak(Box::new(Mutex::new(HashMap::new())));
    let indexer_mnemonic = Arc::new(indexer_mnemonic);

    let (tx, rx) = watch::channel(HashMap::new());

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(_) = indexer_allocations.changed() => {},
                Ok(_) = dispute_manager.changed() => {},
                else => break,
            }

            let allocations = indexer_allocations.borrow().clone();
            let dispute_manager = dispute_manager.borrow().clone();
            let indexer_mnemonic = indexer_mnemonic.clone();

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
                        dispute_manager
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

            // sending updated signers map
            let _ = tx.send(signers.clone());
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use tokio::time::sleep;

    use crate::test_vectors::{
        DISPUTE_MANAGER_ADDRESS, INDEXER_ALLOCATIONS, INDEXER_OPERATOR_MNEMONIC,
    };

    use super::*;

    #[tokio::test]
    async fn test_attestation_signers_update_with_allocations() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (dispute_manager_tx, dispute_manager_rx) = watch::channel(Address::default());

        dispute_manager_tx.send(*DISPUTE_MANAGER_ADDRESS).unwrap();

        let signers_rx = attestation_signers(
            allocations_rx,
            (*INDEXER_OPERATOR_MNEMONIC).to_string(),
            1,
            dispute_manager_rx,
        );

        // Test that an empty set of allocations leads to an empty set of signers
        allocations_tx.send(HashMap::new()).unwrap();
        //change wait if required
        sleep(std::time::Duration::from_millis(50)).await; // waiting for propegation
        let latest_signers = signers_rx.borrow().clone();
        assert_eq!(latest_signers, HashMap::new());

        // Test that writing our set of test allocations results in corresponding signers for all of them
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        sleep(std::time::Duration::from_millis(50)).await; // waiting for propegation
        let latest_signers = signers_rx.borrow().clone();
        assert_eq!(latest_signers.len(), INDEXER_ALLOCATIONS.len());
        for signer_allocation_id in latest_signers.keys() {
            assert!(INDEXER_ALLOCATIONS
                .keys()
                .any(|allocation_id| signer_allocation_id == allocation_id));
        }
    }
}
