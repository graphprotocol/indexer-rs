// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
///
/// This function accepts multiple mnemonics to support allocations created with different
/// operator keys (e.g., after key rotation or migration).
///
/// The `grace_period` parameter controls how long signers are retained after their allocation
/// disappears from the watch channel. This prevents premature eviction during network subgraph
/// polling gaps, ensuring queries with recently-closed allocation IDs can still be signed.
pub fn attestation_signers(
    indexer_allocations_rx: AllocationWatcher,
    indexer_mnemonics: Vec<Mnemonic>,
    chain_id: ChainId,
    dispute_manager_rx: DisputeManagerWatcher,
    grace_period: Duration,
) -> AttestationWatcher {
    let attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>> =
        Box::leak(Box::new(Mutex::new(HashMap::new())));

    // Tombstone map: records when each signer was first evicted
    let evicted_at: &'static Mutex<HashMap<Address, Instant>> =
        Box::leak(Box::new(Mutex::new(HashMap::new())));

    let indexer_mnemonics: Arc<[String]> = indexer_mnemonics
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .into();

    join_and_map_watcher(
        indexer_allocations_rx,
        dispute_manager_rx,
        move |(allocation, dispute)| {
            let indexer_mnemonics = indexer_mnemonics.clone();
            modify_signers(
                &indexer_mnemonics,
                chain_id,
                attestation_signers_map,
                evicted_at,
                grace_period,
                &allocation,
                &dispute,
            )
        },
    )
}

fn modify_signers(
    indexer_mnemonics: &[String],
    chain_id: ChainId,
    attestation_signers_map: &'static Mutex<HashMap<Address, AttestationSigner>>,
    evicted_at: &'static Mutex<HashMap<Address, Instant>>,
    grace_period: Duration,
    allocations: &HashMap<Address, Allocation>,
    dispute_manager: &Address,
) -> HashMap<Address, AttestationSigner> {
    let mut signers = attestation_signers_map.lock().unwrap();
    let mut evicted = evicted_at.lock().unwrap();

    // Retain signers that are active OR within the grace period post-eviction.
    // This prevents premature eviction during network subgraph polling gaps.
    signers.retain(|id, _| {
        if allocations.contains_key(id) {
            // Re-activated: clear tombstone
            evicted.remove(id);
            true
        } else {
            // Record eviction time on first absence; keep until grace period expires
            let first_evicted = evicted.entry(*id).or_insert_with(Instant::now);
            let keep = first_evicted.elapsed() < grace_period;
            if !keep {
                evicted.remove(id); // Clean up tombstone after expiry
            }
            keep
        }
    });

    // Opportunistic tombstone cleanup to prevent unbounded growth
    evicted.retain(|_, t| t.elapsed() < grace_period * 2);

    // Create signers for new allocations
    for (id, allocation) in allocations.iter() {
        if !signers.contains_key(id) {
            tracing::debug!(
                allocation_id = ?allocation.id,
                deployment_id = ?allocation.subgraph_deployment.id,
                created_at_epoch = allocation.created_at_epoch,
                mnemonic_count = indexer_mnemonics.len(),
                "Attempting to create attestation signer for allocation"
            );

            let signer = AttestationSigner::new_with_mnemonics(
                indexer_mnemonics,
                allocation,
                chain_id,
                *dispute_manager,
            );
            match signer {
                Ok(signer) => {
                    tracing::debug!(
                        allocation_id = ?allocation.id,
                        "Successfully created attestation signer for allocation"
                    );
                    signers.insert(*id, signer);
                }
                Err(e) => {
                    tracing::warn!(
                        allocation_id = ?allocation.id,
                        deployment_id = ?allocation.subgraph_deployment.id,
                        created_at_epoch = allocation.created_at_epoch,
                        mnemonic_count = indexer_mnemonics.len(),
                        error = %e,
                        "Failed to establish signer for allocation"
                    );
                    tracing::debug!(
                        allocation_id = ?allocation.id,
                        error = %e,
                        "Signer creation error details"
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
        let grace_period = Duration::from_secs(3600);
        let mut signers = attestation_signers(
            allocations_rx,
            vec![INDEXER_MNEMONIC.clone()],
            1,
            dispute_manager_rx,
            grace_period,
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

    #[tokio::test]
    async fn test_signer_retained_within_grace_period() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (_, dispute_manager_rx) = watch::channel(DISPUTE_MANAGER_ADDRESS);
        // Use a long grace period to ensure signers are retained
        let grace_period = Duration::from_secs(3600);
        let mut signers = attestation_signers(
            allocations_rx,
            vec![INDEXER_MNEMONIC.clone()],
            1,
            dispute_manager_rx,
            grace_period,
        );

        // Add allocations
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        signers.changed().await.unwrap();
        let initial_count = signers.borrow().len();
        assert_eq!(initial_count, INDEXER_ALLOCATIONS.len());

        // Remove all allocations (simulating network subgraph polling gap)
        allocations_tx.send(HashMap::new()).unwrap();
        signers.changed().await.unwrap();

        // Signers should be retained within grace period
        let retained_signers = signers.borrow().clone();
        assert_eq!(
            retained_signers.len(),
            initial_count,
            "Signers should be retained within grace period"
        );
    }

    #[tokio::test]
    async fn test_signer_evicted_after_grace_period() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (_, dispute_manager_rx) = watch::channel(DISPUTE_MANAGER_ADDRESS);
        // Use a very short grace period for testing
        let grace_period = Duration::from_millis(10);
        let mut signers = attestation_signers(
            allocations_rx,
            vec![INDEXER_MNEMONIC.clone()],
            1,
            dispute_manager_rx,
            grace_period,
        );

        // Add allocations
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        signers.changed().await.unwrap();
        assert!(!signers.borrow().is_empty());

        // Remove all allocations
        allocations_tx.send(HashMap::new()).unwrap();
        signers.changed().await.unwrap();

        // Wait for grace period to expire
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Trigger another update to run the retain logic
        allocations_tx.send(HashMap::new()).unwrap();
        signers.changed().await.unwrap();

        // Signers should be evicted after grace period
        assert!(
            signers.borrow().is_empty(),
            "Signers should be evicted after grace period expires"
        );
    }

    #[tokio::test]
    async fn test_reactivated_allocation_clears_tombstone() {
        let (allocations_tx, allocations_rx) = watch::channel(HashMap::new());
        let (_, dispute_manager_rx) = watch::channel(DISPUTE_MANAGER_ADDRESS);
        let grace_period = Duration::from_secs(3600);
        let mut signers = attestation_signers(
            allocations_rx,
            vec![INDEXER_MNEMONIC.clone()],
            1,
            dispute_manager_rx,
            grace_period,
        );

        // Add allocations
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        signers.changed().await.unwrap();
        let initial_count = signers.borrow().len();

        // Remove allocations (starts grace period)
        allocations_tx.send(HashMap::new()).unwrap();
        signers.changed().await.unwrap();

        // Re-add allocations (should clear tombstone)
        allocations_tx.send((*INDEXER_ALLOCATIONS).clone()).unwrap();
        signers.changed().await.unwrap();

        // Signers should still be present
        assert_eq!(
            signers.borrow().len(),
            initial_count,
            "Signers should be retained after re-activation"
        );
    }
}
