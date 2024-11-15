// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use thegraph_core::{Address, DeploymentId};
use tokio::sync::watch::Receiver;

use crate::allocations::Allocation;
use crate::watcher::map_watcher;

/// An always up-to-date list of attestation signers, one for each of the indexer's allocations.
pub fn monitor(
    indexer_allocations_rx: Receiver<HashMap<Address, Allocation>>,
) -> Receiver<HashMap<DeploymentId, Address>> {
    map_watcher(indexer_allocations_rx, move |allocation| {
        allocation
            .iter()
            .map(|(address, allocation)| (allocation.subgraph_deployment.id, *address))
            .collect()
    })
}
