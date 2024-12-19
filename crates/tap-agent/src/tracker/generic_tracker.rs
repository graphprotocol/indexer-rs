// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    time::Duration,
};

use thegraph_core::alloy::primitives::Address;

use super::{
    global_tracker::GlobalTracker, AllocationStats, DefaultFromExtra, DurationInfo, SenderFeeStats,
};
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlobalFeeTracker {
    pub(super) requesting: u128,
    pub(super) total_fee: u128,
}

impl PartialOrd for GlobalFeeTracker {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.get_total_fee().partial_cmp(&other.get_total_fee())
    }
}

#[derive(Debug, Clone, Default)]
pub struct GenericTracker<G, F, E, U>
where
    G: GlobalTracker<U>,
    F: AllocationStats<U> + DefaultFromExtra<E>,
{
    pub(super) global: G,
    pub(super) id_to_fee: HashMap<Address, F>,
    pub(super) extra_data: E,

    _update: PhantomData<U>,
}

impl<G, F, E, U> GenericTracker<G, F, E, U>
where
    G: GlobalTracker<U>,
    U: Copy,
    F: AllocationStats<U> + DefaultFromExtra<E>,
{
    /// Updates and overwrite the fee counter into the specific
    /// value provided.
    ///
    /// IMPORTANT: This function does not affect the buffer window fee
    pub fn update(&mut self, id: Address, value: U) {
        // insert or update, if update remove old fee from total
        let fee = self
            .id_to_fee
            .entry(id)
            .or_insert(F::default_from_extra(&self.extra_data));

        fee.update(value);

        self.recalculate_global();
    }

    fn recalculate_global(&mut self) {
        self.global
            .update(self.id_to_fee.values().map(|fee| fee.get_total_fee()).sum());
    }

    pub fn remove(&mut self, id: Address) {
        if self.id_to_fee.remove(&id).is_some() {
            self.recalculate_global();
        }
    }

    pub fn get_heaviest_allocation_id(&mut self) -> Option<Address> {
        // just loop over and get the biggest fee
        self.id_to_fee
            .iter_mut()
            .filter(|(_, fee)| fee.is_allowed_to_trigger_rav_request())
            .fold(None, |acc: Option<(&Address, u128)>, (addr, value)| {
                if let Some((_, max_fee)) = acc {
                    if value.get_valid_fee() > max_fee {
                        Some((addr, value.get_valid_fee()))
                    } else {
                        acc
                    }
                } else {
                    Some((addr, value.get_valid_fee()))
                }
            })
            .filter(|(_, fee)| *fee > 0)
            .map(|(&id, _)| id)
    }

    pub fn get_list_of_allocation_ids(&self) -> HashSet<Address> {
        self.id_to_fee.keys().cloned().collect()
    }

    pub fn get_total_fee(&self) -> u128 {
        self.global.get_total_fee()
    }

    pub fn get_total_fee_for_allocation(&self, allocation: &Address) -> Option<U> {
        self.id_to_fee.get(allocation).map(|fee| fee.get_stats())
    }
}

impl GenericTracker<GlobalFeeTracker, SenderFeeStats, DurationInfo, UnaggregatedReceipts> {
    pub fn new(buffer_duration: Duration) -> Self {
        Self {
            extra_data: DurationInfo { buffer_duration },
            global: Default::default(),
            id_to_fee: Default::default(),
            _update: Default::default(),
        }
    }

    /// Adds into the total_fee entry and buffer window totals
    ///
    /// It's important to notice that `value` cannot be less than
    /// zero, so the only way to make this counter lower is by using
    /// `update` function
    pub fn add(&mut self, id: Address, value: u128, timestamp_ns: u64) {
        let contains_buffer = self.contains_buffer();
        let entry = self
            .id_to_fee
            .entry(id)
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));

        // fee
        self.global.total_fee += value;
        entry.total_fee += value;

        // counter for allocation
        entry.count += 1;

        if contains_buffer {
            entry.buffer_info.new_entry(value, timestamp_ns);
        }
    }

    fn contains_buffer(&self) -> bool {
        self.extra_data.buffer_duration > Duration::ZERO
    }

    pub fn get_ravable_total_fee(&mut self) -> u128 {
        self.get_total_fee()
            - self.global.requesting
            - self.get_buffered_fee().min(self.global.total_fee)
    }

    fn get_buffered_fee(&mut self) -> u128 {
        self.id_to_fee
            .values_mut()
            .fold(0u128, |acc, expiring| acc + expiring.buffer_info.get_sum())
    }

    pub fn get_count_outside_buffer_for_allocation(&mut self, allocation_id: &Address) -> u64 {
        self.id_to_fee
            .get_mut(allocation_id)
            .map(|alloc| alloc.ravable_count())
            .unwrap_or_default()
    }

    pub fn start_rav_request(&mut self, allocation_id: Address) {
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));
        entry.requesting = entry.total_fee;
        self.global.requesting += entry.requesting;
    }

    /// Should be called before `update`
    pub fn finish_rav_request(&mut self, allocation_id: Address) {
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));
        self.global.requesting -= entry.requesting;
        entry.requesting = 0;
    }

    pub fn ok_rav_request(&mut self, allocation_id: Address) {
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));
        entry.backoff_info.ok();
    }

    pub fn failed_rav_backoff(&mut self, allocation_id: Address) {
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));
        entry.backoff_info.fail();
    }
}

impl<G> GenericTracker<G, SenderFeeStats, DurationInfo, UnaggregatedReceipts>
where
    G: GlobalTracker<UnaggregatedReceipts>,
{
    pub fn block_allocation_id(&mut self, address: Address) {
        self.id_to_fee.entry(address).and_modify(|v| {
            v.blocked = true;
        });
    }

    pub fn unblock_allocation_id(&mut self, address: Address) {
        self.id_to_fee.entry(address).and_modify(|v| {
            v.blocked = false;
        });
    }

    pub fn can_trigger_rav(&self, allocation_id: Address) -> bool {
        self.id_to_fee
            .get(&allocation_id)
            .map(|alloc| alloc.is_allowed_to_trigger_rav_request())
            .unwrap_or_default()
    }
}

impl AllocationStats<u128> for u128 {
    fn update(&mut self, v: u128) {
        *self = v;
    }

    fn is_allowed_to_trigger_rav_request(&self) -> bool {
        *self > 0
    }

    fn get_stats(&self) -> u128 {
        *self
    }

    fn get_valid_fee(&mut self) -> u128 {
        self.get_stats()
    }

    fn get_total_fee(&self) -> u128 {
        *self
    }
}
