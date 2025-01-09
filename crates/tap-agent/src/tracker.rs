// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

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

use crate::unaggregated_receipts::UnaggregatedReceipts;

pub type SimpleFeeTracker = GenericTracker<u128, u128, NoExtraData, u128>;
pub type SenderFeeTracker =
    GenericTracker<GlobalFeeTracker, SenderFeeStats, DurationInfo, UnaggregatedReceipts>;

pub trait AllocationStats<U> {
    fn update(&mut self, v: U);
    fn is_allowed_to_trigger_rav_request(&self) -> bool;
    fn get_stats(&self) -> U;
    fn get_total_fee(&self) -> u128;
    fn get_valid_fee(&mut self) -> u128;
}
