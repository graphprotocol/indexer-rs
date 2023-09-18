// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use ethers_core::types::U256;
use log::{error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::prelude::{AllocationMonitor, AttestationSigner};

use super::{attestation_signer_for_allocation, signer::create_attestation_signer};

#[derive(Debug, Clone)]
pub struct AttestationSigners {
    inner: Arc<AttestationSignersInner>,
    _update_loop_handle: Arc<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
pub(crate) struct AttestationSignersInner {
    attestation_signers: Arc<RwLock<HashMap<Address, AttestationSigner>>>,
    allocation_monitor: AllocationMonitor,
    indexer_mnemonic: String,
    chain_id: U256,
    dispute_manager: Address,
}

impl AttestationSigners {
    pub fn new(
        allocation_monitor: AllocationMonitor,
        indexer_mnemonic: String,
        chain_id: U256,
        dispute_manager: Address,
    ) -> Self {
        let inner = Arc::new(AttestationSignersInner {
            attestation_signers: Arc::new(RwLock::new(HashMap::new())),
            allocation_monitor,
            indexer_mnemonic,
            chain_id,
            dispute_manager,
        });

        let _update_loop_handle = {
            let inner = inner.clone();
            tokio::spawn(Self::update_loop(inner.clone()))
        };

        Self {
            inner,
            _update_loop_handle: Arc::new(_update_loop_handle),
        }
    }

    pub(crate) async fn update_attestation_signers(inner: Arc<AttestationSignersInner>) {
        let mut attestation_signers_write = inner.attestation_signers.write().await;
        for allocation in inner
            .allocation_monitor
            .get_eligible_allocations()
            .await
            .values()
        {
            if let std::collections::hash_map::Entry::Vacant(e) =
                attestation_signers_write.entry(allocation.id)
            {
                match attestation_signer_for_allocation(&inner.indexer_mnemonic, allocation)
                    .and_then(|signer| {
                        create_attestation_signer(
                            inner.chain_id,
                            inner.dispute_manager,
                            signer,
                            allocation.subgraph_deployment.id.bytes32(),
                        )
                    }) {
                    Ok(signer) => {
                        e.insert(signer);
                        info!(
                            "Found attestation signer for {{allocation: {}, deployment: {}}}",
                            allocation.id,
                            allocation.subgraph_deployment.id.ipfs_hash()
                        );
                    }
                    Err(e) => {
                        warn!(
                        "Failed to find the attestation signer for {{allocation: {}, deployment: {}, createdAtEpoch: {}, err: {}}}",
                        allocation.id, allocation.subgraph_deployment.id.ipfs_hash(), allocation.created_at_epoch, e
                    )
                    }
                }
            }
        }
    }

    async fn update_loop(inner: Arc<AttestationSignersInner>) {
        let mut watch_receiver = inner.allocation_monitor.subscribe();

        loop {
            match watch_receiver.changed().await {
                Ok(_) => {
                    Self::update_attestation_signers(inner.clone()).await;
                }
                Err(e) => {
                    error!(
                        "Error receiving allocation monitor subscription update: {}",
                        e
                    );
                }
            }
        }
    }

    pub async fn read(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, HashMap<Address, AttestationSigner>> {
        self.inner.attestation_signers.read().await
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use ethers_core::types::U256;
    use std::str::FromStr;
    use std::sync::Arc;

    use crate::prelude::AllocationMonitor;
    use crate::test_vectors;

    use super::*;

    #[tokio::test]
    async fn test_update_attestation_signers() {
        unsafe {
            let mut mock_allocation_monitor = AllocationMonitor::faux();

            faux::when!(mock_allocation_monitor.get_eligible_allocations).then_unchecked(|_| {
                // Spawn a thread to be able to call `blocking_read` on the RwLock, which actually spins its own async
                // runtime.
                // This is needed because `faux` will also use a runtime to mock the async function.
                let t = std::thread::spawn(|| {
                    let eligible_allocations = Box::leak(Box::new(Arc::new(RwLock::new(
                        test_vectors::expected_eligible_allocations(),
                    ))));
                    eligible_allocations.blocking_read()
                });
                t.join().unwrap()
            });

            let inner = Arc::new(AttestationSignersInner {
                attestation_signers: Arc::new(RwLock::new(HashMap::new())),
                allocation_monitor: mock_allocation_monitor,
                indexer_mnemonic: test_vectors::INDEXER_OPERATOR_MNEMONIC.to_string(),
                chain_id: U256::from(1),
                dispute_manager: Address::from_str(test_vectors::DISPUTE_MANAGER_ADDRESS).unwrap(),
            });

            AttestationSigners::update_attestation_signers(inner.clone()).await;

            // Check that the attestation signers were found for the allocations
            assert_eq!(inner.attestation_signers.read().await.len(), 4);
        }
    }
}
