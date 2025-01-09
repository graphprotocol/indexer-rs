// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use super::GlobalFeeTracker;
use crate::unaggregated_receipts::UnaggregatedReceipts;

pub trait GlobalTracker<T>: Sized {
    fn get_total_fee(&self) -> u128;
    fn update(&mut self, new_fee: u128);
}

impl GlobalTracker<u128> for u128 {
    fn get_total_fee(&self) -> u128 {
        *self
    }

    fn update(&mut self, new_fee: u128) {
        *self = new_fee;
    }
}

impl GlobalTracker<UnaggregatedReceipts> for GlobalFeeTracker {
    fn get_total_fee(&self) -> u128 {
        self.total_fee
    }

    fn update(&mut self, new_fee: u128) {
        self.total_fee = new_fee;
    }
}
