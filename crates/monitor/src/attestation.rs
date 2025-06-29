// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bip39::Mnemonic;
use indexer_allocation::Allocation;
use indexer_attestation::AttestationSigner;
use indexer_watcher::join_and_map_watcher;
use thegraph_core::alloy::primitives::{Address, ChainId};
use tokio::sync::watch::Receiver;

use crate::{AllocationWatcher, DisputeManagerWatcher};

/// Receiver for Map of allocation id and attestation signer
pub type AttestationWatcher = Receiver<HashMap<Address, AttestationSigner>>;

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn attestation_signers(
    indexer_allocations_rx: AllocationWatcher,
    indexer_mnemonic: Mnemonic,
    chain_id: ChainId,
    dispute_manager_rx: DisputeManagerWatcher,
) -> AttestationWatcher {
    let attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>> =
        Box::leak(Box::new(Mutex::new(HashMap::new())));
    let indexer_mnemonic = Arc::new(indexer_mnemonic.to_string());

    join_and_map_watcher(
        indexer_allocations_rx,
        dispute_manager_rx,
        move |(allocation, dispute)| {
            let indexer_mnemonic = indexer_mnemonic.clone();
            modify_sigers(
                &indexer_mnemonic,
                chain_id,
                attestation_signers_map,
                &allocation,
                &dispute,
            )
        },
    )
}
fn modify_sigers(
    indexer_mnemonic: &str,
    chain_id: ChainId,
    attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>>,
    allocations: &HashMap<Address, Allocation>,
    dispute_manager: &Address,
) -> HashMap<Address, AttestationSigner> {
    let mut signers = attestation_signers_map.lock().unwrap();
    // Remove signers for allocations that are no longer active or recently closed
    signers.retain(|id, _| allocations.contains_key(id));

    // Create signers for new allocations
    for (id, allocation) in allocations.iter() {
        if !signers.contains_key(id) {
            tracing::debug!(
                "Attempting to create attestation signer for allocation {}, deployment {}, createdAtEpoch {}",
                allocation.id, allocation.subgraph_deployment.id, allocation.created_at_epoch
            );

            let signer =
                AttestationSigner::new(indexer_mnemonic, allocation, chain_id, *dispute_manager);
            match signer {
                Ok(signer) => {
                    tracing::debug!(
                        "Successfully created attestation signer for allocation {}",
                        allocation.id
                    );
                    signers.insert(*id, signer);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to establish signer for allocation {}, deployment {}, createdAtEpoch {}: {}",
                        allocation.id, allocation.subgraph_deployment.id,
                        allocation.created_at_epoch, e
                    );
                    tracing::debug!(
                        "Signer creation error details for allocation {}: {}",
                        allocation.id,
                        e
                    );
                }
            }
        }
    }

    signers.clone()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use test_assets::{DISPUTE_MANAGER_ADDRESS, INDEXER_ALLOCATIONS, INDEXER_MNEMONIC};
    use tokio::sync::watch;

    use super::*;

    #[tokio::test]
    async fn test_attestation_signers_update_with_allocations() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (_, dispute_manager_rx) = watch::channel(DISPUTE_MANAGER_ADDRESS);
        let mut signers = attestation_signers(
            allocations_rx,
            INDEXER_MNEMONIC.clone(),
            1,
            dispute_manager_rx,
        );

        // Test that an empty set of allocations leads to an empty set of signers
        allocations_tx.send(HashMap::new()).unwrap();
        signers.changed().await.unwrap();
        let latest_signers = signers.borrow().clone();
        assert_eq!(latest_signers, HashMap::new());

        // Test that writing our set of test allocations results in corresponding signers for all of them
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        signers.changed().await.unwrap();
        let latest_signers = signers.borrow().clone();
        assert_eq!(latest_signers.len(), INDEXER_ALLOCATIONS.len());

        for signer_allocation_id in latest_signers.keys() {
            assert!(INDEXER_ALLOCATIONS
                .keys()
                .any(|allocation_id| signer_allocation_id == allocation_id));
        }
    }
}
