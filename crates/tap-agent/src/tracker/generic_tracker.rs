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

/// Global Fee Tracker used inside SenderFeeTracker
///
/// This allows to keep a simple O(1) instead of looping over all allocations to get the total
/// amount
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
    pub(crate) global: G,
    pub(crate) id_to_fee: HashMap<Address, F>,
    pub(crate) extra_data: E,

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

    #[tracing::instrument(skip(self), ret, level = "trace")]
    pub fn get_heaviest_allocation_id(&mut self) -> Option<Address> {
        let total_allocations = self.id_to_fee.len();
        tracing::debug!(total_allocations, "Evaluating allocations for RAV request",);

        if total_allocations == 0 {
            tracing::warn!("No allocations found in fee tracker");
            return None;
        }

        let mut eligible_count = 0;
        let mut zero_valid_fee_count = 0;

        let result = self
            .id_to_fee
            .iter_mut()
            .filter(|(addr, fee)| {
                let allowed = fee.is_allowed_to_trigger_rav_request();
                if allowed {
                    eligible_count += 1;
                }

                tracing::trace!(
                    allocation = %addr,
                    allowed,
                    "Allocation eligibility check",
                );

                allowed
            })
            .fold(None, |acc: Option<(&Address, u128)>, (addr, value)| {
                let current_fee = value.get_valid_fee();

                tracing::trace!(
                    allocation = %addr,
                    valid_fee = %current_fee,
                    "Checking allocation valid fees",
                );

                if current_fee == 0 {
                    zero_valid_fee_count += 1;
                }

                if let Some((current_addr, max_fee)) = acc {
                    if current_fee > max_fee {
                        tracing::trace!(
                            new = %addr,
                            new_fee = %current_fee,
                            old = %current_addr,
                            old_fee = %max_fee,
                            "New heaviest allocation",
                        );
                        Some((addr, current_fee))
                    } else {
                        acc
                    }
                } else if current_fee > 0 {
                    tracing::trace!(
                        allocation = %addr,
                        valid_fee = %current_fee,
                        "First valid allocation",
                    );
                    Some((addr, current_fee))
                } else {
                    acc
                }
            })
            .filter(|(_, fee)| {
                let valid = *fee > 0;
                tracing::trace!(fee = %fee, valid, "Final fee check");
                valid
            })
            .map(|(&id, fee)| {
                tracing::info!(allocation = %id, fee = %fee, "Selected heaviest allocation");
                id
            });

        if result.is_none() {
            tracing::warn!(
                total_allocations,
                eligible_count,
                zero_valid_fee_count,
                "No valid allocation found",
            );

            // Additional logging for SenderFeeTracker specifically
            if std::any::type_name::<F>().contains("SenderFeeStats") {
                tracing::debug!("This appears to be a SenderFeeTracker - checking buffer status");
                for (addr, fee) in &mut self.id_to_fee {
                    let total_fee = fee.get_total_fee();
                    let valid_fee = fee.get_valid_fee();
                    let buffered_fee = total_fee - valid_fee;

                    tracing::debug!(
                        allocation = %addr,
                        total_fee,
                        valid_fee,
                        buffered_fee,
                        "Allocation fee breakdown",
                    );
                }
            }
        }

        result
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
        // Use saturating_sub to prevent underflow when requesting or buffered fees
        // exceed total fee (can happen after RAV success resets counters)
        let total_fee = self.get_total_fee();
        let requesting_fee = self.global.requesting;
        let raw_buffered_fee = self.get_buffered_fee();
        let buffered_fee = raw_buffered_fee.min(total_fee);

        let result = total_fee
            .saturating_sub(requesting_fee)
            .saturating_sub(buffered_fee);

        // Log only when the pre-min raw buffered fee or requesting exceeds total
        // TODO: Investigate if this edge case is expected behavior.
        // It may occur right after a successful RAV resets totals to zero while
        // some receipts are still within the buffer window awaiting expiration.
        if requesting_fee > total_fee || raw_buffered_fee > total_fee {
            // This can happen when a RAV completes (resetting totals) but receipts are still in the buffer
            tracing::warn!(
                total_fee = total_fee,
                requesting_fee = requesting_fee,
                raw_buffered_fee = raw_buffered_fee,
                buffered_fee = buffered_fee,
                result = result,
                "Fees exceed total fee - using saturating arithmetic to prevent underflow"
            );
        }

        result
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
