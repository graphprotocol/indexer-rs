// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

pub use extra_data::{DefaultFromExtra, DurationInfo, NoExtraData};
use generic_tracker::{GenericTracker, GlobalTracker};
pub use sender_fee_stats::SenderFeeStats;

mod extra_data;
mod generic_tracker;
mod sender_fee_stats;
#[cfg(test)]
mod tracker_tests;

pub type SimpleFeeTracker<T, E = NoExtraData> = GenericTracker<T, E>;

pub trait AllocationStats<G: GlobalTracker> {
    fn update(&mut self, v: G);
    fn should_filter_out(&self) -> bool;
    fn get_fee(&self) -> G;
    fn get_valid_fee(&mut self) -> G;
}
