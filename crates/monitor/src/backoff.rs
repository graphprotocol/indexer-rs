// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Tiered block backoff strategy for handling Gateway routing to indexers with varying sync states.
//!
//! When querying subgraphs through the Gateway, different indexers may be at different block heights.
//! This module provides a backoff strategy that progressively relaxes the `number_gte` constraint
//! when queries fail due to indexers being behind.

use std::sync::LazyLock;

use regex::Regex;

/// Backoff tiers representing how many blocks to subtract from the last successful block.
/// Each tier represents a progressively more lenient constraint:
/// - Tier 0: Exact block (no backoff)
/// - Tier 1: 100 blocks back (~25 sec on Arbitrum)
/// - Tier 2: 1,000 blocks back (~4 min on Arbitrum)
const BACKOFF_TIERS: [i64; 3] = [0, 100, 1_000];

/// Regex to extract `latest: \d+` values from Gateway error messages.
/// Gateway errors look like:
/// ```text
/// bad indexers: {0x123...: Unavailable(missing block: 900000000, latest: 425638278), ...}
/// ```
static LATEST_BLOCK_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"latest:\s*(\d+)").expect("Invalid regex pattern"));

/// Tiered backoff strategy for block-constrained queries.
///
/// Tracks the last successful block number and progressively relaxes the `number_gte`
/// constraint when queries fail due to indexers being behind the requested block.
#[derive(Debug, Clone, Default)]
pub struct TieredBlockBackoff {
    /// The block number from the last successful query
    last_successful_block: Option<i64>,
    /// Current backoff tier (0 = no backoff, higher = more lenient)
    tier: usize,
    /// Maximum "latest" block extracted from Gateway error messages
    max_latest_from_error: Option<i64>,
}

impl TieredBlockBackoff {
    /// Creates a new backoff state with no prior block information.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the `number_gte` value to use for the next query.
    ///
    /// - If no successful block is recorded, returns `None` (no constraint)
    /// - If within normal tiers, returns `last_block - BACKOFF_TIERS[tier]`
    /// - If all tiers exhausted, falls back to `max_latest_from_error`
    pub fn get_number_gte(&self) -> Option<i64> {
        let base = self.last_successful_block?;

        if self.tier < BACKOFF_TIERS.len() {
            Some((base - BACKOFF_TIERS[self.tier]).max(0))
        } else {
            // All tiers exhausted, use the max latest from error if available
            self.max_latest_from_error
        }
    }

    /// Called when a query succeeds. Records the block number and resets the tier.
    pub fn on_success(&mut self, block_number: i64) {
        self.last_successful_block = Some(block_number);
        self.tier = 0;
        self.max_latest_from_error = None;
    }

    /// Called when a query fails due to a "block too high" error.
    ///
    /// Parses the error message to extract the maximum available block number
    /// and advances to the next backoff tier.
    pub fn on_block_too_high_error(&mut self, error: &str) {
        if let Some(max_latest) = parse_max_latest(error) {
            self.max_latest_from_error = Some(
                self.max_latest_from_error
                    .map_or(max_latest, |current| current.max(max_latest)),
            );
        }
        self.tier = self.tier.saturating_add(1);
    }

    /// Returns the current backoff tier (0 = no backoff).
    pub fn current_tier(&self) -> usize {
        self.tier
    }

    /// Returns the last successful block number, if any.
    pub fn last_successful_block(&self) -> Option<i64> {
        self.last_successful_block
    }

    /// Returns true if all normal tiers have been exhausted.
    pub fn is_exhausted(&self) -> bool {
        self.tier >= BACKOFF_TIERS.len()
    }
}

/// Parses a Gateway error message to extract the maximum "latest" block number.
///
/// Gateway errors contain entries like `latest: 425638278` for each indexer that
/// couldn't serve the requested block. This function extracts all such values
/// and returns the maximum.
pub fn parse_max_latest(error: &str) -> Option<i64> {
    LATEST_BLOCK_REGEX
        .captures_iter(error)
        .filter_map(|cap| cap.get(1)?.as_str().parse::<i64>().ok())
        .max()
}

/// Checks if an error message indicates a "block too high" / "missing block" error
/// from the Gateway.
pub fn is_block_too_high_error(error: &str) -> bool {
    error.contains("missing block") || error.contains("block not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_backoff_returns_none() {
        let backoff = TieredBlockBackoff::new();
        assert_eq!(backoff.get_number_gte(), None);
        assert_eq!(backoff.current_tier(), 0);
    }

    #[test]
    fn test_success_sets_block_and_resets_tier() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.tier = 3;
        backoff.on_success(1000);

        assert_eq!(backoff.last_successful_block(), Some(1000));
        assert_eq!(backoff.current_tier(), 0);
        assert_eq!(backoff.get_number_gte(), Some(1000));
    }

    #[test]
    fn test_tier_progression() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.on_success(10000);

        // Tier 0: exact block
        assert_eq!(backoff.get_number_gte(), Some(10000));

        // Tier 1: -100
        backoff.on_block_too_high_error("some error");
        assert_eq!(backoff.current_tier(), 1);
        assert_eq!(backoff.get_number_gte(), Some(9900));

        // Tier 2: -1000
        backoff.on_block_too_high_error("some error");
        assert_eq!(backoff.current_tier(), 2);
        assert_eq!(backoff.get_number_gte(), Some(9000));
    }

    #[test]
    fn test_exhausted_tiers_uses_max_latest() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.on_success(1_000_000);

        // Exhaust all tiers
        for _ in 0..4 {
            backoff.on_block_too_high_error("latest: 500000");
        }

        assert!(backoff.is_exhausted());
        assert_eq!(backoff.get_number_gte(), Some(500000));
    }

    #[test]
    fn test_parse_max_latest_single_value() {
        let error =
            "bad indexers: {0x123: Unavailable(missing block: 900000000, latest: 425638278)}";
        assert_eq!(parse_max_latest(error), Some(425638278));
    }

    #[test]
    fn test_parse_max_latest_multiple_values() {
        let error = "bad indexers: {0x123: Unavailable(missing block: 900000000, latest: 425638278), 0x456: Unavailable(missing block: 900000000, latest: 425638300)}";
        assert_eq!(parse_max_latest(error), Some(425638300));
    }

    #[test]
    fn test_parse_max_latest_no_match() {
        let error = "some other error message";
        assert_eq!(parse_max_latest(error), None);
    }

    #[test]
    fn test_parse_max_latest_with_spaces() {
        let error = "latest:  425638278, latest: 100";
        assert_eq!(parse_max_latest(error), Some(425638278));
    }

    #[test]
    fn test_is_block_too_high_error() {
        assert!(is_block_too_high_error(
            "Unavailable(missing block: 900000000)"
        ));
        assert!(is_block_too_high_error("block not found"));
        assert!(!is_block_too_high_error("connection timeout"));
    }

    #[test]
    fn test_saturating_sub_prevents_negative() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.on_success(500); // Small block number

        // Tier 2 would subtract 1000, but should saturate to 0
        backoff.tier = 2;
        assert_eq!(backoff.get_number_gte(), Some(0));
    }

    #[test]
    fn test_max_latest_accumulates_highest() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.on_success(1_000_000);

        backoff.on_block_too_high_error("latest: 100");
        assert_eq!(backoff.max_latest_from_error, Some(100));

        backoff.on_block_too_high_error("latest: 500");
        assert_eq!(backoff.max_latest_from_error, Some(500));

        backoff.on_block_too_high_error("latest: 200");
        assert_eq!(backoff.max_latest_from_error, Some(500)); // Should keep 500
    }

    #[test]
    fn test_success_clears_max_latest() {
        let mut backoff = TieredBlockBackoff::new();
        backoff.on_success(1000);
        backoff.on_block_too_high_error("latest: 500");

        assert_eq!(backoff.max_latest_from_error, Some(500));

        backoff.on_success(2000);
        assert_eq!(backoff.max_latest_from_error, None);
    }
}
