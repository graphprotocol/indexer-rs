// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Freshness tracking for periodic subgraph queries.
//!
//! This module provides utilities to detect and handle stale data from subgraph queries,
//! protecting against Gateway routing to indexers that are significantly behind.
//!
//! # Design Principles
//!
//! 1. **Caller decides** — Returns structured results, caller handles appropriately
//! 2. **Lenient on init** — First data is always accepted (best_timestamp starts at 0)
//! 3. **Stale-but-improvement accepted** — If we have old data, newer-but-still-stale is better
//! 4. **Escalation** — Warn on stale, error if no fresh data for extended period

use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Default threshold for escalating from warning to error level logging.
/// If no fresh data is received for this duration, log at error level.
const DEFAULT_ESCALATION_THRESHOLD: Duration = Duration::from_secs(2 * 3600); // 2 hours

/// Result of freshness validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessResult {
    /// Data is within the freshness threshold
    Fresh,
    /// Data is stale but fresher than current best (an improvement)
    StaleButImprovement {
        /// Age of the data in minutes
        age_mins: u64,
    },
    /// Data is stale and not fresher than current best (rejected)
    StaleRejected {
        /// Age of the data in minutes
        age_mins: u64,
    },
}

impl FreshnessResult {
    /// Returns true if data should be accepted (Fresh or StaleButImprovement)
    #[allow(dead_code)] // Useful for callers, used in tests
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Fresh | Self::StaleButImprovement { .. })
    }

    /// Returns true if data is fresh (within threshold)
    #[allow(dead_code)] // Useful for callers, used in tests
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }
}

/// Tracks freshness state for periodic subgraph queries.
///
/// Maintains:
/// - `best_timestamp`: The most recent block timestamp we've seen
/// - `last_fresh_update`: When we last received truly fresh data
///
/// Thread-safe via atomic operations.
#[derive(Debug)]
pub struct FreshnessTracker {
    /// Best (most recent) block timestamp seen
    best_timestamp: AtomicI64,
    /// Unix timestamp of when we last received fresh data
    last_fresh_update: AtomicI64,
    /// Maximum allowed staleness in minutes
    max_staleness_mins: u64,
    /// Threshold for escalating to error-level logging
    escalation_threshold: Duration,
}

impl FreshnessTracker {
    /// Creates a new tracker with the given staleness threshold.
    ///
    /// # Arguments
    /// * `max_staleness_mins` - Maximum allowed age of data in minutes. Set to 0 to disable checks.
    pub fn new(max_staleness_mins: u64) -> Self {
        Self {
            best_timestamp: AtomicI64::new(0),
            last_fresh_update: AtomicI64::new(0),
            max_staleness_mins,
            escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
        }
    }

    /// Creates a new tracker with custom escalation threshold.
    #[cfg(test)]
    pub fn with_escalation_threshold(
        max_staleness_mins: u64,
        escalation_threshold: Duration,
    ) -> Self {
        Self {
            best_timestamp: AtomicI64::new(0),
            last_fresh_update: AtomicI64::new(0),
            max_staleness_mins,
            escalation_threshold,
        }
    }

    /// Returns true if staleness checking is enabled.
    pub fn is_enabled(&self) -> bool {
        self.max_staleness_mins > 0
    }

    /// Checks freshness of new data and updates tracking state if accepted.
    ///
    /// # Arguments
    /// * `block_timestamp` - The block timestamp from the subgraph response
    ///
    /// # Returns
    /// * `Some(FreshnessResult)` - Result of freshness check
    /// * `None` - If staleness checking is disabled or timestamp is invalid
    ///
    /// # Side Effects
    /// - Updates `best_timestamp` if data is accepted
    /// - Updates `last_fresh_update` if data is fresh
    pub fn check_and_update(&self, block_timestamp: Option<i64>) -> Option<FreshnessResult> {
        if !self.is_enabled() {
            // Staleness check disabled, but still track best timestamp
            if let Some(ts) = block_timestamp {
                if ts > 0 {
                    self.best_timestamp.fetch_max(ts, Ordering::SeqCst);
                }
            }
            return None;
        }

        let Some(new_timestamp) = block_timestamp else {
            // No timestamp - can't validate, accept by default
            tracing::warn!("Subgraph response missing block timestamp, cannot validate freshness");
            return None;
        };

        // Guard against invalid timestamps
        if new_timestamp <= 0 {
            tracing::warn!(
                block_timestamp = new_timestamp,
                "Subgraph response has invalid block timestamp"
            );
            return None;
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();
        let block_age_secs = now_secs.saturating_sub(new_timestamp as u64);
        let max_staleness_secs = self.max_staleness_mins * 60;
        let age_mins = block_age_secs / 60;

        let is_fresh = block_age_secs <= max_staleness_secs;
        let current_best = self.best_timestamp.load(Ordering::SeqCst);
        let is_improvement = new_timestamp > current_best;

        if is_fresh {
            // Fresh data always wins
            self.best_timestamp
                .fetch_max(new_timestamp, Ordering::SeqCst);
            self.last_fresh_update
                .store(now_secs as i64, Ordering::SeqCst);
            Some(FreshnessResult::Fresh)
        } else if is_improvement {
            // Stale but better than what we have
            self.best_timestamp
                .fetch_max(new_timestamp, Ordering::SeqCst);
            Some(FreshnessResult::StaleButImprovement { age_mins })
        } else {
            // Stale and not an improvement - reject
            Some(FreshnessResult::StaleRejected { age_mins })
        }
    }

    /// Returns the duration since last fresh data was received.
    ///
    /// Returns `None` if no fresh data has ever been received.
    pub fn time_since_fresh_update(&self) -> Option<Duration> {
        let last_fresh = self.last_fresh_update.load(Ordering::SeqCst);
        if last_fresh == 0 {
            return None;
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs() as i64;

        let elapsed_secs = now_secs.saturating_sub(last_fresh);
        Some(Duration::from_secs(elapsed_secs as u64))
    }

    /// Checks if we should escalate logging to error level.
    ///
    /// Returns true if fresh data hasn't been received for longer than the escalation threshold.
    pub fn should_escalate(&self) -> bool {
        self.time_since_fresh_update()
            .is_some_and(|elapsed| elapsed > self.escalation_threshold)
    }

    /// Returns the current best timestamp.
    pub fn best_timestamp(&self) -> i64 {
        self.best_timestamp.load(Ordering::SeqCst)
    }

    /// Returns the configured max staleness in minutes.
    pub fn max_staleness_mins(&self) -> u64 {
        self.max_staleness_mins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn now_minus_secs(secs: u64) -> i64 {
        now_secs() - secs as i64
    }

    #[test]
    fn test_fresh_data_accepted() {
        let tracker = FreshnessTracker::new(30); // 30 min threshold
        let ts = now_minus_secs(60); // 1 minute ago

        let result = tracker.check_and_update(Some(ts));

        assert_eq!(result, Some(FreshnessResult::Fresh));
        assert_eq!(tracker.best_timestamp(), ts);
        assert!(tracker.time_since_fresh_update().is_some());
    }

    #[test]
    fn test_stale_but_improvement_accepted() {
        let tracker = FreshnessTracker::new(30);

        // First: very old data (2 hours ago) - accepted as first data
        let old_ts = now_minus_secs(7200);
        let result1 = tracker.check_and_update(Some(old_ts));
        // First data is stale but improvement over 0
        assert!(result1.unwrap().is_accepted());

        // Second: less old data (1 hour ago) - accepted as improvement
        let newer_ts = now_minus_secs(3600);
        let result2 = tracker.check_and_update(Some(newer_ts));

        assert!(matches!(
            result2,
            Some(FreshnessResult::StaleButImprovement { .. })
        ));
        assert_eq!(tracker.best_timestamp(), newer_ts);
    }

    #[test]
    fn test_stale_no_improvement_rejected() {
        let tracker = FreshnessTracker::new(30);

        // First: moderately old data (1 hour ago)
        let first_ts = now_minus_secs(3600);
        tracker.check_and_update(Some(first_ts));

        // Second: even older data (2 hours ago) - rejected
        let older_ts = now_minus_secs(7200);
        let result = tracker.check_and_update(Some(older_ts));

        assert!(matches!(
            result,
            Some(FreshnessResult::StaleRejected { .. })
        ));
        // Best timestamp unchanged
        assert_eq!(tracker.best_timestamp(), first_ts);
    }

    #[test]
    fn test_disabled_tracker() {
        let tracker = FreshnessTracker::new(0); // Disabled
        let ts = now_minus_secs(999999); // Very old

        let result = tracker.check_and_update(Some(ts));

        assert!(result.is_none()); // No result when disabled
        assert_eq!(tracker.best_timestamp(), ts); // But still tracks
    }

    #[test]
    fn test_missing_timestamp() {
        let tracker = FreshnessTracker::new(30);

        let result = tracker.check_and_update(None);

        assert!(result.is_none());
        assert_eq!(tracker.best_timestamp(), 0);
    }

    #[test]
    fn test_invalid_timestamp() {
        let tracker = FreshnessTracker::new(30);

        let result = tracker.check_and_update(Some(-1));

        assert!(result.is_none());
        assert_eq!(tracker.best_timestamp(), 0);
    }

    #[test]
    fn test_escalation_not_triggered_initially() {
        let tracker = FreshnessTracker::new(30);

        // No fresh data received yet
        assert!(!tracker.should_escalate());
        assert!(tracker.time_since_fresh_update().is_none());
    }

    #[test]
    fn test_escalation_not_triggered_after_fresh() {
        let tracker = FreshnessTracker::new(30);
        let ts = now_minus_secs(60); // Fresh data

        tracker.check_and_update(Some(ts));

        assert!(!tracker.should_escalate());
        assert!(tracker.time_since_fresh_update().unwrap() < Duration::from_secs(5));
    }

    #[test]
    fn test_escalation_triggered_after_threshold() {
        // Use short escalation threshold for testing
        let tracker = FreshnessTracker::with_escalation_threshold(30, Duration::from_secs(1));

        // Manually set last_fresh_update to simulate old fresh data
        let old_fresh = now_secs() - 10; // 10 seconds ago
        tracker.last_fresh_update.store(old_fresh, Ordering::SeqCst);

        // Should escalate since 10 seconds > 1 second threshold
        assert!(tracker.should_escalate());
    }

    #[test]
    fn test_fresh_resets_escalation() {
        let tracker = FreshnessTracker::with_escalation_threshold(30, Duration::from_secs(1));

        // Simulate old fresh update
        let old_fresh = now_secs() - 10;
        tracker.last_fresh_update.store(old_fresh, Ordering::SeqCst);
        assert!(tracker.should_escalate());

        // New fresh data resets escalation
        let fresh_ts = now_minus_secs(60);
        tracker.check_and_update(Some(fresh_ts));

        assert!(!tracker.should_escalate());
    }

    #[test]
    fn test_stale_improvement_does_not_reset_escalation() {
        let tracker = FreshnessTracker::with_escalation_threshold(30, Duration::from_secs(1));

        // Set up: had fresh data long ago
        let old_fresh = now_secs() - 10;
        tracker.last_fresh_update.store(old_fresh, Ordering::SeqCst);
        tracker
            .best_timestamp
            .store(now_minus_secs(7200), Ordering::SeqCst);

        // Stale-but-improvement doesn't reset escalation
        let stale_ts = now_minus_secs(3600); // 1 hour old, but better than 2 hours
        let result = tracker.check_and_update(Some(stale_ts));

        assert!(matches!(
            result,
            Some(FreshnessResult::StaleButImprovement { .. })
        ));
        assert!(tracker.should_escalate()); // Still escalated
    }

    #[test]
    fn test_init_accepts_very_stale_data() {
        let tracker = FreshnessTracker::new(30);

        // Very old data (1.5 years) on init - accepted because it's > 0
        let ancient_ts = now_minus_secs(47_000_000);
        let result = tracker.check_and_update(Some(ancient_ts));

        // Should be StaleButImprovement (improvement over initial 0)
        assert!(result.unwrap().is_accepted());
        assert_eq!(tracker.best_timestamp(), ancient_ts);
    }
}
