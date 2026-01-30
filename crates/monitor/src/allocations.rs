// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use indexer_allocation::Allocation;
use indexer_query::allocations_query::{self, AllocationsQuery};
use indexer_watcher::new_watcher;
use thegraph_core::alloy::primitives::{Address, TxHash};
use tokio::sync::watch::Receiver;

use crate::client::SubgraphClient;

/// Receiver of Map between allocation id and allocation struct
pub type AllocationWatcher = Receiver<HashMap<Address, Allocation>>;

/// Response from allocation query including metadata for freshness validation.
#[derive(Debug)]
pub struct AllocationQueryResponse {
    /// The allocations indexed by their address
    pub allocations: HashMap<Address, Allocation>,
    /// Block number from `_meta.block.number`
    pub block_number: Option<i64>,
    /// Block timestamp from `_meta.block.timestamp` (Unix seconds)
    pub block_timestamp: Option<i64>,
}

/// An always up-to-date list of an indexer's active and recently closed allocations.
///
/// # Arguments
/// * `network_subgraph` - The subgraph client to query
/// * `indexer_address` - The indexer's address
/// * `interval` - How often to poll for updates
/// * `recently_closed_allocation_buffer` - How long to keep closed allocations
/// * `max_data_staleness_mins` - Maximum allowed age of data in minutes. Set to 0 to disable.
///   When data is older than this threshold, the update is rejected unless it's fresher than
///   the current best data. This protects against Gateway routing to stale indexers.
///
/// # Freshness Behavior
/// Data is accepted if ANY of these conditions are met:
/// - Data is fresh (within `max_data_staleness_mins` threshold)
/// - Data is stale but fresher than the current best (an improvement)
///
/// This ensures:
/// - Fresh data always wins (even if it shows 0 allocations — that's the current truth)
/// - Stale-but-better data replaces stale-and-worse data
/// - Stale-and-worse data is rejected to preserve better data
pub async fn indexer_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    recently_closed_allocation_buffer: Duration,
    max_data_staleness_mins: u64,
) -> anyhow::Result<AllocationWatcher> {
    // Track the best (most recent) block timestamp we've seen
    // Initialized to 0 so the first fetch is always accepted
    let best_timestamp = Arc::new(AtomicI64::new(0));

    new_watcher(interval, move || {
        let best_timestamp = best_timestamp.clone();
        async move {
            let response = get_allocations_with_metadata(
                network_subgraph,
                indexer_address,
                recently_closed_allocation_buffer,
            )
            .await?;

            // Validate freshness and update best timestamp if accepted
            if max_data_staleness_mins > 0 {
                check_and_update_freshness(&response, max_data_staleness_mins, &best_timestamp)?;
            } else {
                // Staleness check disabled, but still track best timestamp
                if let Some(ts) = response.block_timestamp {
                    best_timestamp.fetch_max(ts, Ordering::SeqCst);
                }
            }

            Ok(response.allocations)
        }
    })
    .await
}

/// Checks data freshness and updates the best known timestamp if data is accepted.
///
/// # Acceptance Rules
/// Data is accepted if ANY of these conditions are met:
/// 1. Data is fresh (within `max_staleness_mins` threshold)
/// 2. Data is stale but fresher than `best_timestamp` (an improvement over current)
///
/// If accepted, `best_timestamp` is updated to the new timestamp.
/// If rejected, returns an error and `best_timestamp` remains unchanged.
fn check_and_update_freshness(
    response: &AllocationQueryResponse,
    max_staleness_mins: u64,
    best_timestamp: &AtomicI64,
) -> anyhow::Result<()> {
    let Some(new_timestamp) = response.block_timestamp else {
        // No timestamp in response - log warning but accept (can't validate)
        // This could happen with older subgraph versions
        tracing::warn!(
            block_number = ?response.block_number,
            "Network subgraph response missing block timestamp, cannot validate freshness"
        );
        return Ok(());
    };

    // Guard against invalid timestamps (negative or zero)
    if new_timestamp <= 0 {
        tracing::warn!(
            block_timestamp = new_timestamp,
            block_number = ?response.block_number,
            "Network subgraph response has invalid block timestamp"
        );
        return Ok(());
    }

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();
    let block_age_secs = now_secs.saturating_sub(new_timestamp as u64);
    let max_staleness_secs = max_staleness_mins * 60;
    let age_mins = block_age_secs / 60;

    let is_fresh = block_age_secs <= max_staleness_secs;
    let current_best = best_timestamp.load(Ordering::SeqCst);
    let is_improvement = new_timestamp > current_best;

    if is_fresh {
        // Fresh data always wins
        best_timestamp.fetch_max(new_timestamp, Ordering::SeqCst);
        tracing::debug!(
            block_number = ?response.block_number,
            block_timestamp = new_timestamp,
            age_minutes = age_mins,
            "Accepted fresh network subgraph data"
        );
        Ok(())
    } else if is_improvement {
        // Stale but better than what we have
        best_timestamp.fetch_max(new_timestamp, Ordering::SeqCst);
        tracing::info!(
            block_number = ?response.block_number,
            block_timestamp = new_timestamp,
            previous_best_timestamp = current_best,
            age_minutes = age_mins,
            "Accepted stale network subgraph data (fresher than previous best)"
        );
        Ok(())
    } else {
        // Stale and not an improvement - reject
        tracing::warn!(
            block_number = ?response.block_number,
            block_timestamp = new_timestamp,
            current_best_timestamp = current_best,
            age_minutes = age_mins,
            max_staleness_minutes = max_staleness_mins,
            "Rejecting stale network subgraph data (not fresher than current best)"
        );
        Err(anyhow::anyhow!(
            "Network subgraph data is {} minutes old (block {} at timestamp {}), \
             not fresher than current best (timestamp {}), max allowed: {} minutes",
            age_mins,
            response.block_number.unwrap_or(-1),
            new_timestamp,
            current_best,
            max_staleness_mins
        ))
    }
}

/// Fetches allocations from the network subgraph with metadata for freshness validation.
///
/// This function does NOT perform staleness validation - that is the caller's responsibility.
/// Use `validate_freshness()` to check if the response is fresh enough.
pub async fn get_allocations_with_metadata(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    recently_closed_allocation_buffer: Duration,
) -> Result<AllocationQueryResponse, anyhow::Error> {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let closed_at_threshold = since_the_epoch - recently_closed_allocation_buffer;

    let mut hash: Option<TxHash> = None;
    let mut block_number: Option<i64> = None;
    let mut block_timestamp: Option<i64> = None;
    let mut last: Option<String> = None;
    let mut responses = vec![];
    let page_size = 200;
    loop {
        let result = network_subgraph
            .query::<AllocationsQuery, _>(allocations_query::Variables {
                indexer: indexer_address.to_string().to_ascii_lowercase(),
                closed_at_threshold: closed_at_threshold.as_secs() as i64,
                first: page_size,
                last: last.unwrap_or_default(),
                block: hash.map(|hash| allocations_query::Block_height {
                    hash: Some(hash),
                    number: None,
                    number_gte: None,
                }),
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut data = result?;
        let page_len = data.allocations.len();

        // Capture block info from meta (first page sets it, subsequent pages use hash for consistency)
        if let Some(ref meta) = data.meta {
            if block_number.is_none() {
                block_number = Some(meta.block.number);
                block_timestamp = meta.block.timestamp;
            }
        }
        hash = data.meta.and_then(|meta| meta.block.hash);
        last = data.allocations.last().map(|entry| entry.id.to_string());

        responses.append(&mut data.allocations);
        if (page_len as i64) < page_size {
            break;
        }
    }

    let allocations: HashMap<Address, Allocation> = responses
        .into_iter()
        .map(|allocation| allocation.try_into())
        .collect::<Result<Vec<Allocation>, _>>()?
        .into_iter()
        .map(|allocation| (allocation.id, allocation))
        .collect();

    tracing::info!(
        allocations = allocations.len(),
        block_number = ?block_number,
        block_timestamp = ?block_timestamp,
        indexer_address = ?indexer_address,
        "Network subgraph query returned allocations for indexer"
    );

    Ok(AllocationQueryResponse {
        allocations,
        block_number,
        block_timestamp,
    })
}

/// Convenience wrapper that fetches allocations without metadata.
///
/// This is useful for callers that don't need freshness validation.
#[allow(dead_code)] // Used in tests and kept as public API for external callers
pub async fn get_allocations(
    network_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    recently_closed_allocation_buffer: Duration,
) -> Result<HashMap<Address, Allocation>, anyhow::Error> {
    let response = get_allocations_with_metadata(
        network_subgraph,
        indexer_address,
        recently_closed_allocation_buffer,
    )
    .await?;
    Ok(response.allocations)
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

    fn now_minus_secs(secs: u64) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - secs as i64
    }

    #[test]
    fn test_freshness_accepts_fresh_data() {
        let best_ts = AtomicI64::new(0);
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: Some(now_minus_secs(60)), // 1 minute ago
        };
        assert!(check_and_update_freshness(&response, 30, &best_ts).is_ok());
        assert!(best_ts.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn test_freshness_rejects_stale_non_improvement() {
        // Start with a good timestamp (10 mins ago)
        let best_ts = AtomicI64::new(now_minus_secs(600));
        let old_best = best_ts.load(Ordering::SeqCst);

        // New data is 1 hour old - stale AND older than current best
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: Some(now_minus_secs(3600)),
        };
        let result = check_and_update_freshness(&response, 30, &best_ts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("minutes old"));
        // Best timestamp should remain unchanged
        assert_eq!(best_ts.load(Ordering::SeqCst), old_best);
    }

    #[test]
    fn test_freshness_accepts_stale_improvement() {
        // Start with very old timestamp (2 hours ago)
        let best_ts = AtomicI64::new(now_minus_secs(7200));

        // New data is 1 hour old - stale but FRESHER than current best
        let new_ts = now_minus_secs(3600);
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: Some(new_ts),
        };
        assert!(check_and_update_freshness(&response, 30, &best_ts).is_ok());
        // Best timestamp should be updated
        assert_eq!(best_ts.load(Ordering::SeqCst), new_ts);
    }

    #[test]
    fn test_freshness_handles_missing_timestamp() {
        let best_ts = AtomicI64::new(1000);
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: None,
        };
        // Should not fail, just warn
        assert!(check_and_update_freshness(&response, 30, &best_ts).is_ok());
        // Best timestamp unchanged (can't update without new timestamp)
        assert_eq!(best_ts.load(Ordering::SeqCst), 1000);
    }

    #[test]
    fn test_freshness_handles_invalid_timestamp() {
        let best_ts = AtomicI64::new(1000);
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: Some(-1),
        };
        // Should not fail on invalid timestamp
        assert!(check_and_update_freshness(&response, 30, &best_ts).is_ok());
    }

    #[test]
    fn test_freshness_init_accepts_any_data() {
        // Simulates initialization: best_ts starts at 0
        let best_ts = AtomicI64::new(0);

        // Even very stale data (1.5 years old) should be accepted on init
        // because it's still > 0 (improvement over initial state)
        let old_ts = now_minus_secs(47_000_000); // ~1.5 years
        let response = AllocationQueryResponse {
            allocations: HashMap::new(),
            block_number: Some(100),
            block_timestamp: Some(old_ts),
        };
        assert!(check_and_update_freshness(&response, 30, &best_ts).is_ok());
        assert_eq!(best_ts.load(Ordering::SeqCst), old_ts);
    }
}
