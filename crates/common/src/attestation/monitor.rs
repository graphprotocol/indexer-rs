// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bip39::Mnemonic;
use std::sync::Arc;
use std::{collections::HashMap, sync::Mutex};
use thegraph_core::{Address, ChainId};
use tokio::sync::watch::Receiver;
use tracing::warn;

use crate::{attestation::signer::AttestationSigner, watcher::join_and_map_watcher};
use indexer_allocation::Allocation;

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn attestation_signers(
    indexer_allocations_rx: Receiver<HashMap<Address, Allocation>>,
    indexer_mnemonic: Mnemonic,
    chain_id: ChainId,
    dispute_manager_rx: Receiver<Address>,
) -> Receiver<HashMap<Address, AttestationSigner>> {
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
) -> HashMap<thegraph_core::Address, AttestationSigner> {
    let mut signers = attestation_signers_map.lock().unwrap();
    // Remove signers for allocations that are no longer active or recently closed
    signers.retain(|id, _| allocations.contains_key(id));

    // Create signers for new allocations
    for (id, allocation) in allocations.iter() {
        if !signers.contains_key(id) {
            let signer =
                AttestationSigner::new(indexer_mnemonic, allocation, chain_id, *dispute_manager);
            match signer {
                Ok(signer) => {
                    signers.insert(*id, signer);
                }
                Err(e) => {
                    warn!(
                        "Failed to establish signer for allocation {}, deployment {}, createdAtEpoch {}: {}",
                        allocation.id, allocation.subgraph_deployment.id,
                        allocation.created_at_epoch, e
                    );
                }
            }
        }
    }

    signers.clone()
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;

    use crate::test_vectors::{DISPUTE_MANAGER_ADDRESS, INDEXER_ALLOCATIONS, INDEXER_MNEMONIC};

    use super::*;

    #[tokio::test]
    async fn test_attestation_signers_update_with_allocations() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (_, dispute_manager_rx) = watch::channel(*DISPUTE_MANAGER_ADDRESS);
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
