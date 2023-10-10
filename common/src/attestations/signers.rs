// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use ethers_core::types::U256;
use eventuals::{Eventual, EventualExt};
use log::warn;
use lru::LruCache;
use std::sync::Arc;
use std::{collections::HashMap, num::NonZeroUsize};
use tokio::sync::Mutex;

use crate::prelude::{Allocation, AttestationSigner};

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn attestation_signers(
    indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    indexer_mnemonic: String,
    chain_id: U256,
    dispute_manager: Address,
) -> Eventual<HashMap<Address, AttestationSigner>> {
    // Keep a cache of the most recent 1000 signers around so we don't need to recreate them
    // every time there is a small change in the allocations
    let cache: &'static Mutex<LruCache<_, _>> = Box::leak(Box::new(Mutex::new(LruCache::new(
        NonZeroUsize::new(1000).unwrap(),
    ))));

    let indexer_mnemonic = Arc::new(indexer_mnemonic);

    // Whenever the indexer's active or recently closed allocations change, make sure
    // we have attestation signers for all of them
    indexer_allocations.map(move |allocations| {
        let indexer_mnemonic = indexer_mnemonic.clone();

        async move {
            let mut cache = cache.lock().await;

            for (id, allocation) in allocations.iter() {
                let result = cache.try_get_or_insert(*id, || {
                    AttestationSigner::new(&indexer_mnemonic, allocation, chain_id, dispute_manager)
                });

                if let Err(e) = result {
                    warn!(
                        "Failed to establish signer for allocation {}, deployment {}, createdAtEpoch {}: {}",
                        allocation.id, allocation.subgraph_deployment.id,
                        allocation.created_at_epoch, e
                    );
                }
            }

            HashMap::from_iter(cache.iter().map(|(k, v)| (*k, v.clone())))
        }
    })
}
