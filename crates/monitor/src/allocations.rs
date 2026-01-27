// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use indexer_allocation::Allocation;
use indexer_query::allocations_query::{self, AllocationsQuery};
use thegraph_core::alloy::primitives::{Address, TxHash};
use tokio::{
    sync::{watch::Receiver, Mutex},
    time::{self, sleep},
};

use crate::{
    backoff::{is_block_too_high_error, TieredBlockBackoff},
    client::SubgraphClient,
};

/// Receiver of Map between allocation id and allocation struct
pub type AllocationWatcher = Receiver<HashMap<Address, Allocation>>;

/// An always up-to-date list of an indexer's active and recently closed allocations.
///
/// Uses tiered backoff to handle Gateway routing to indexers with varying sync states.
/// When queries fail due to "block too high" errors, the `number_gte` constraint is
/// progressively relaxed to find an indexer that can serve the request.
pub async fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    recently_closed_allocation_buffer: Duration,
) -> anyhow::Result<AllocationWatcher> {
    let backoff = Arc::new(Mutex::new(TieredBlockBackoff::new()));

    // Initial query without number_gte constraint
    let initial_result = get_allocations_with_block_state(
        network_subgraph,
        indexer_address,
        recently_closed_allocation_buffer,
        None,
    )
    .await?;

    // Record the initial block number for subsequent queries
    if let Some(block_number) = initial_result.block_number {
        backoff.lock().await.on_success(block_number);
    }

    let (tx, rx) = tokio::sync::watch::channel(initial_result.allocations);

    let backoff_clone = backoff.clone();
    tokio::spawn(async move {
        let mut time_interval = time::interval(interval);
        time_interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        // Consume the immediate first tick to avoid redundant query at startup
        time_interval.tick().await;

        loop {
            time_interval.tick().await;

            let number_gte = backoff_clone.lock().await.get_number_gte();

            let result = get_allocations_with_block_state(
                network_subgraph,
                indexer_address,
                recently_closed_allocation_buffer,
                number_gte,
            )
            .await;

            match result {
                Ok(query_result) => {
                    // Check if we should update before any async operations
                    // This prevents overwriting valid data with empty results from stale indexers
                    let (should_update, current_count) = {
                        let current = tx.borrow();
                        let should_update =
                            !query_result.allocations.is_empty() || current.is_empty();
                        (should_update, current.len())
                    };

                    if should_update {
                        if let Some(block_number) = query_result.block_number {
                            backoff_clone.lock().await.on_success(block_number);
                        }

                        if tx.send(query_result.allocations).is_err() {
                            tracing::debug!(
                                "Allocation watcher channel closed, stopping watcher task"
                            );
                            break;
                        }
                    } else {
                        tracing::warn!(
                            current_count = current_count,
                            new_count = query_result.allocations.len(),
                            "Ignoring empty allocation result to preserve existing data"
                        );
                    }
                }
                Err(err) => {
                    let error_str = err.to_string();

                    if is_block_too_high_error(&error_str) {
                        let mut backoff_guard = backoff_clone.lock().await;
                        backoff_guard.on_block_too_high_error(&error_str);

                        tracing::warn!(
                            error = %err,
                            tier = backoff_guard.current_tier(),
                            number_gte = ?backoff_guard.get_number_gte(),
                            "Block too high error, advancing backoff tier"
                        );
                    } else {
                        tracing::warn!(
                            error = %err,
                            "Error while updating allocation watcher"
                        );
                    }

                    // Sleep before retry
                    sleep(interval.div_f32(2.0)).await;
                }
            }
        }
    });

    Ok(rx)
}

/// Result of an allocation query including block metadata for freshness tracking.
pub struct AllocationQueryResult {
    pub allocations: HashMap<Address, Allocation>,
    pub block_number: Option<i64>,
}

pub async fn get_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    recently_closed_allocation_buffer: Duration,
) -> Result<HashMap<Address, Allocation>, anyhow::Error> {
    let result = get_allocations_with_block_state(
        network_subgraph,
        indexer_address,
        recently_closed_allocation_buffer,
        None,
    )
    .await?;
    Ok(result.allocations)
}

/// Queries allocations with block freshness tracking.
///
/// Accepts an optional `number_gte` constraint to ensure the query is served by an indexer
/// at least as fresh as the specified block. Returns both the allocations and the block
/// number from the response for use in subsequent queries.
pub async fn get_allocations_with_block_state(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    recently_closed_allocation_buffer: Duration,
    number_gte: Option<i64>,
) -> Result<AllocationQueryResult, anyhow::Error> {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let closed_at_threshold = since_the_epoch - recently_closed_allocation_buffer;

    let mut hash: Option<TxHash> = None;
    let mut block_number: Option<i64> = None;
    let mut last: Option<String> = None;
    let mut responses = vec![];
    let page_size = 200;

    loop {
        // For the first page, use number_gte if provided; for subsequent pages, use hash
        let block_constraint = if hash.is_some() {
            Some(allocations_query::Block_height {
                hash,
                number: None,
                number_gte: None,
            })
        } else {
            number_gte.map(|n| allocations_query::Block_height {
                hash: None,
                number: None,
                number_gte: Some(n),
            })
        };

        let result = network_subgraph
            .query::<AllocationsQuery, _>(allocations_query::Variables {
                indexer: indexer_address.to_string().to_ascii_lowercase(),
                closed_at_threshold: closed_at_threshold.as_secs() as i64,
                first: page_size,
                last: last.unwrap_or_default(),
                block: block_constraint,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut data = result?;
        let page_len = data.allocations.len();

        // Extract block metadata from response
        if let Some(ref meta) = data.meta {
            hash = meta.block.hash;
            block_number = Some(meta.block.number);
        }
        last = data.allocations.last().map(|entry| entry.id.to_string());

        responses.append(&mut data.allocations);
        if (page_len as i64) < page_size {
            break;
        }
    }

    let responses = responses
        .into_iter()
        .map(|allocation| allocation.try_into())
        .collect::<Result<Vec<Allocation>, _>>()?;

    let allocations: HashMap<Address, Allocation> = responses
        .into_iter()
        .map(|allocation| (allocation.id, allocation))
        .collect();

    tracing::info!(
        allocations = allocations.len(),
        block_number = ?block_number,
        indexer_address = ?indexer_address,
        "Network subgraph query returned allocations for indexer"
    );

    Ok(AllocationQueryResult {
        allocations,
        block_number,
    })
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use thegraph_core::alloy::primitives::address;

    use super::*;
    use crate::client::{DeploymentDetails, SubgraphClient};

    async fn network_subgraph_client() -> &'static SubgraphClient {
        let url = std::env::var("NETWORK_SUBGRAPH_URL").unwrap();
        Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&url).unwrap(),
            )
            .await,
        ))
    }

    #[tokio::test]
    #[test_with::env(NETWORK_SUBGRAPH_URL)]
    async fn test_network_query() {
        let result = get_allocations(
            network_subgraph_client().await,
            address!("326c584e0f0eab1f1f83c93cc6ae1acc0feba0bc"),
            Duration::from_secs(1712448507),
        )
        .await;
        assert!(result.unwrap().len() > 2000)
    }

    #[tokio::test]
    #[test_with::env(NETWORK_SUBGRAPH_URL)]
    async fn test_network_query_empty_response() {
        let result = get_allocations(
            network_subgraph_client().await,
            address!("deadbeefcafebabedeadbeefcafebabedeadbeef"),
            Duration::from_secs(1712448507),
        )
        .await
        .unwrap();
        assert!(result.is_empty())
    }
}
