// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # Service Constants
//!
//! Centralized configuration constants for the indexer-service crate.
//!
//! This module consolidates all magic numbers and configuration defaults
//! to improve discoverability, documentation, and maintainability.
//!
//! ## Design Rationale
//!
//! Constants are grouped by functional area:
//! - **HTTP**: Network timeouts and client configuration
//! - **Database**: Connection pool settings
//! - **Rate Limiting**: Request throttling parameters
//! - **TAP**: Receipt processing configuration
//! - **Status Queries**: GraphQL validation limits
//! - **Monitoring**: Background task intervals
//!
//! ## Future Improvements
//!
//! Many of these constants are candidates for runtime configuration.

use std::time::Duration;

// =============================================================================
// HTTP CLIENT CONFIGURATION
// =============================================================================

/// Default timeout for HTTP client requests.
///
/// Applied to the main `reqwest::Client` used for graph-node queries,
/// subgraph requests, and general HTTP operations.
///
/// 30 seconds provides reasonable tolerance for:
/// - Complex subgraph queries
/// - Network latency spikes
/// - Graph-node processing time
pub const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for DIPS HTTP client requests.
///
/// DIPS (Decentralized Indexer Payment System) operations involve:
/// - IPFS content fetching
/// - Agreement validation and storage
/// - Network registry lookups
///
/// 60 seconds provides additional headroom for these heavier operations
/// compared to standard graph-node queries.
pub const DIPS_HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

// =============================================================================
// DATABASE CONFIGURATION
// =============================================================================

/// Maximum time to wait when acquiring a database connection from the pool.
///
/// If no connection becomes available within this duration, the operation
/// fails with a timeout error. This prevents request pile-up during
/// database issues.
///
/// 30 seconds balances:
/// - Allowing slow queries to complete
/// - Failing fast enough to surface connection pool exhaustion
pub const DATABASE_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of connections in the database pool.
///
/// This limits concurrent database operations. The value should be tuned
/// based on:
/// - PostgreSQL `max_connections` setting
/// - Expected concurrent request load
/// - Number of service replicas sharing the database
///
/// 50 connections is a reasonable default for a single-instance deployment.
/// Production deployments should configure this based on load testing.
pub const DATABASE_MAX_CONNECTIONS: u32 = 50;

// =============================================================================
// RATE LIMITING CONFIGURATION
// =============================================================================

/// Burst size for miscellaneous endpoints (health, metrics, etc.).
///
/// Allows this many requests immediately before rate limiting kicks in.
/// Lower than query endpoints since these are typically automated checks.
pub const MISC_RATE_LIMIT_BURST_SIZE: u32 = 10;

/// Rate limit replenish interval for miscellaneous endpoints.
///
/// After burst is exhausted, one request is allowed per this interval.
/// 100ms = 10 requests/second sustained rate.
pub const MISC_RATE_LIMIT_REPLENISH_INTERVAL: Duration = Duration::from_millis(100);

/// Burst size for static subgraph endpoints (network, escrow subgraphs).
///
/// Higher than misc endpoints since these serve query traffic.
pub const STATIC_SUBGRAPH_RATE_LIMIT_BURST_SIZE: u32 = 50;

/// Rate limit replenish interval for static subgraph endpoints.
///
/// After burst is exhausted, one request is allowed per this interval.
/// 20ms = 50 requests/second sustained rate.
pub const STATIC_SUBGRAPH_RATE_LIMIT_REPLENISH_INTERVAL: Duration = Duration::from_millis(20);

// =============================================================================
// TAP RECEIPT PROCESSING
// =============================================================================

/// Grace period for TAP receipt value checks.
///
/// When validating minimum receipt values against cost models, receipts
/// within this grace period of their timestamp are allowed even if
/// the cost model has changed. This prevents race conditions where
/// a receipt is generated just before a cost model update.
///
/// 60 seconds provides buffer for:
/// - Network propagation delays
/// - Clock skew between gateway and indexer
/// - Cost model update propagation
pub const TAP_RECEIPT_GRACE_PERIOD: Duration = Duration::from_secs(60);

/// Maximum number of receipts that can be queued for storage.
///
/// The receipt storage pipeline uses an async channel to decouple
/// receipt validation from database writes. This sets the channel
/// capacity.
///
/// If the queue fills (database writes slower than receipt arrival),
/// new receipts will apply backpressure to the validation pipeline.
///
/// 1000 receipts provides buffer for:
/// - Database write latency spikes
/// - Batch write efficiency (writes happen in batches)
pub const TAP_RECEIPT_MAX_QUEUE_SIZE: usize = 1000;

/// Batch size for receipt database inserts.
///
/// Receipts are accumulated into batches of this size before
/// being written to the database in a single transaction.
///
/// Larger batches improve throughput but increase latency for
/// individual receipts. 100 balances these concerns.
pub const TAP_RECEIPT_STORAGE_BATCH_SIZE: usize = 100;

// =============================================================================
// STATUS QUERY VALIDATION
// =============================================================================

/// Maximum allowed size for status query payloads.
///
/// Rejects GraphQL status queries larger than this to prevent:
/// - Memory exhaustion from parsing huge queries
/// - DoS attacks via query complexity
///
/// 4KB is generous for legitimate status queries, which are typically
/// under 500 bytes. Complex queries with many fields rarely exceed 2KB.
pub const STATUS_QUERY_MAX_SIZE_BYTES: usize = 4096;

/// Maximum nesting depth for status query selection sets.
///
/// Limits recursion depth when validating GraphQL queries to prevent:
/// - Stack overflow from deeply nested fragments
/// - Query complexity attacks
///
/// 10 levels is far more than any legitimate query needs while
/// providing protection against malicious inputs.
pub const STATUS_QUERY_MAX_SELECTION_DEPTH: usize = 10;

// =============================================================================
// MONITORING AND BACKGROUND TASKS
// =============================================================================

/// Polling interval for dispute manager updates.
///
/// The dispute manager address is fetched from the network subgraph
/// and cached. This interval controls how often we check for updates.
///
/// 1 hour is appropriate because:
/// - Dispute manager changes are rare (governance events)
/// - Stale data has minimal operational impact
/// - Reduces load on network subgraph
pub const DISPUTE_MANAGER_POLL_INTERVAL: Duration = Duration::from_secs(3600);

// =============================================================================
// ROUTE PATHS
// =============================================================================

/// Default route path for nested routers.
pub const DEFAULT_ROUTE_PATH: &str = "/";
