// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use thegraph_core::{Address, DeploymentId};
use tokio::sync::watch::Receiver;

use indexer_allocation::Allocation;
use indexer_watcher::map_watcher;

/// Watcher of indexer allocation
/// returning a map of subgraph deployment to allocation id
pub fn deployment_to_allocation(
    indexer_allocations_rx: Receiver<HashMap<Address, Allocation>>,
) -> Receiver<HashMap<DeploymentId, Address>> {
    map_watcher(indexer_allocations_rx, move |allocation| {
        allocation
            .iter()
            .map(|(address, allocation)| (allocation.subgraph_deployment.id, *address))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;

    use super::deployment_to_allocation;

    #[tokio::test]
    async fn test_deployment_to_allocation() {
        let allocations = test_assets::INDEXER_ALLOCATIONS.clone();
        let allocations_watcher = watch::channel(allocations.clone()).1;
        let deployment = deployment_to_allocation(allocations_watcher);

        let deployments = deployment.borrow();
        // one of the allocation id point to the same subgraph
        assert_eq!(deployments.len(), 3);
        // check if all allocations point to the subgraph id
        for (key, val) in deployments.iter() {
            assert_eq!(allocations.get(val).unwrap().subgraph_deployment.id, *key);
        }
    }
}
