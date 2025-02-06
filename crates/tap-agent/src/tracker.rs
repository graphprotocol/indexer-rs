// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # tracker
//!
//! This is the heart of tap-agent, every decision depends on these trackers.
//!
//! There are two main trackers: [SimpleFeeTracker] [SenderFeeTracker].
//! Each of them enable certain methods that allows fine-grained control over what are the
//! algorithm that selects the biggest allocation.
//!
//! The tracker contains a tricky feature called buffer that is used to count know the counter of
//! any key in X amount of seconds ago. This is important since, receipt timestamp_ns is provided
//! by senders, we want to have a tolerance buffer where we still accept receipts with timestamp
//! older than our current clock.

pub use extra_data::{DefaultFromExtra, DurationInfo, NoExtraData};
use generic_tracker::GenericTracker;
pub use sender_fee_stats::SenderFeeStats;

mod extra_data;
mod generic_tracker;
mod global_tracker;
mod sender_fee_stats;
#[cfg(test)]
mod tracker_tests;

pub use generic_tracker::GlobalFeeTracker;

use crate::agent::unaggregated_receipts::UnaggregatedReceipts;

/// Simple Tracker used for just `u128` fees and no extra blocking or unblocking feature
///
/// It's mainly used for Invalid Receipts and Ravs, since they only need to store and retrieve the
/// total amount of all allocations combined
pub type SimpleFeeTracker = GenericTracker<u128, u128, NoExtraData, u128>;
/// SenderFeeTracker used for more complex features.
///
/// It uses [UnaggregatedReceipts] instead of [u128], contains a buffer for selection, and contains
/// more logic about if an allocation is available for selection or not.
pub type SenderFeeTracker =
    GenericTracker<GlobalFeeTracker, SenderFeeStats, DurationInfo, UnaggregatedReceipts>;

/// Stats trait used by the Counter of a given allocation.
///
/// This is the data that goes in the Value side of the Map inside our Tracker
pub trait AllocationStats<U> {
    /// updates its value with a new one
    fn update(&mut self, v: U);
    /// Returns if an allocation is allows to trigger a rav request
    fn is_allowed_to_trigger_rav_request(&self) -> bool;
    /// Get the stats U given
    fn get_stats(&self) -> U;
    /// Returns the total fee (validated and pending)
    fn get_total_fee(&self) -> u128;
    /// Returns only ravable fees (no pending fees) that is used for triggering
    ///
    /// Pending fees are usually fees that not eligible to trigger a RAV,
    /// for example, you don't want to trigger a Rav Request if your only allocation is currently
    /// requesting, so this should return a value that doesn't contain that allocation
    fn get_valid_fee(&mut self) -> u128;
}
