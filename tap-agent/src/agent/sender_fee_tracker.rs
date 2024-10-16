// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::Address;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::{AddAssign, SubAssign},
    time::{Duration, Instant},
};
use tracing::error;

pub trait UpdateValue<T> {
    fn update(&mut self, v: T, count: u64);
    fn should_filter_out(&self) -> bool;
    fn get_fee(&self) -> T;
    fn get_valid_fee(&mut self) -> T;
}

#[derive(Debug, Clone, Default)]
pub struct BufferedReceiptFee {
    total_fee: u128,
    count: u64,
    // there are some allocations that we don't want it to be
    // heaviest allocation, because they are already marked for finalization,
    // and thus requesting RAVs on their own in their `post_stop` routine.
    blocked: bool,

    requesting: bool,

    // buffer
    entries: VecDeque<(Instant, u128)>,
    fee_in_buffer: u128,
    duration: Duration,
    failed_info: FailedRavInfo,
}

#[derive(Debug, Clone)]
pub struct FailedRavInfo {
    failed_ravs_count: u32,
    failed_rav_backoff_time: Instant,
}

impl BufferedReceiptFee {
    fn get_sum(&mut self) -> u128 {
        self.cleanup();
        self.fee_in_buffer
    }

    fn get_count(&mut self) -> u64 {
        self.cleanup();
        self.entries.len() as u64
    }

    fn cleanup(&mut self) {
        let now = Instant::now();
        while let Some(&(timestamp, value)) = self.entries.front() {
            if now.duration_since(timestamp) >= self.duration {
                self.entries.pop_front();
                self.fee_in_buffer -= value;
            } else {
                break;
            }
        }
    }
}

impl UpdateValue<u128> for BufferedReceiptFee {
    fn update(&mut self, v: u128, count: u64) {
        self.total_fee = v;
        self.count = count;
    }

    fn should_filter_out(&self) -> bool {
        let now = Instant::now();
        let in_backoff = now > self.failed_info.failed_rav_backoff_time;

        in_backoff || self.blocked || self.requesting
    }

    fn get_fee(&self) -> u128 {
        self.total_fee
    }

    fn get_valid_fee(&mut self) -> u128 {
        self.total_fee - self.get_sum().min(self.total_fee)
    }
}

impl AddAssign<u128> for BufferedReceiptFee {
    fn add_assign(&mut self, rhs: u128) {
        self.total_fee += rhs;
    }
}

impl AddAssign for BufferedReceiptFee {
    fn add_assign(&mut self, rhs: Self) {
        self.total_fee += rhs.total_fee;
    }
}

impl SubAssign for BufferedReceiptFee {
    fn sub_assign(&mut self, rhs: Self) {
        self.total_fee -= rhs.total_fee;
    }
}

pub trait ShouldRemove {
    fn should_remove(&self) -> bool;
}

#[derive(Debug, Clone, Default)]
pub struct GenericTracker<F, E> {
    id_to_fee: HashMap<Address, F>,
    total_fee: u128,
    fees_requesting: u128,
    extra_data: E,
}

impl UpdateValue<u128> for u128 {
    fn update(&mut self, v: u128, _count: u64) {
        *self = v;
    }

    fn should_filter_out(&self) -> bool {
        *self == 0
    }

    fn get_fee(&self) -> u128 {
        *self
    }

    fn get_valid_fee(&mut self) -> u128 {
        self.get_fee()
    }
}

impl ShouldRemove for u128 {
    fn should_remove(&self) -> bool {
        *self == 0
    }
}

impl AddAssign<BufferedReceiptFee> for u128 {
    fn add_assign(&mut self, rhs: BufferedReceiptFee) {
        *self = self.checked_add(rhs.total_fee).unwrap_or_else(|| {
            // This should never happen, but if it does, we want to know about it.
            error!(
                "Overflow when adding receipt value {} to total fee {}. \
                    Setting total fee to u128::MAX.",
                self, rhs.total_fee
            );
            u128::MAX
        });
    }
}

impl SubAssign<BufferedReceiptFee> for u128 {
    fn sub_assign(&mut self, rhs: BufferedReceiptFee) {
        *self -= rhs.total_fee;
    }
}

impl Default for FailedRavInfo {
    fn default() -> Self {
        Self {
            failed_ravs_count: 0,
            failed_rav_backoff_time: Instant::now(),
        }
    }
}

#[derive(Default)]
pub struct NoExtraData;

pub type SenderFeeTracker<T, E = NoExtraData> = GenericTracker<T, E>;

#[derive(Default, Debug, Clone)]
pub struct DurationInfo {
    buffer_duration: Duration,
}

pub trait DefaultFromExtra<E> {
    fn default_from_extra(extra: &E) -> Self;
}

impl<T> DefaultFromExtra<NoExtraData> for T
where
    T: Default,
{
    fn default_from_extra(_: &NoExtraData) -> Self {
        Default::default()
    }
}

impl DefaultFromExtra<DurationInfo> for BufferedReceiptFee {
    fn default_from_extra(extra: &DurationInfo) -> Self {
        BufferedReceiptFee {
            duration: extra.buffer_duration,
            ..Default::default()
        }
    }
}

impl<F, E> GenericTracker<F, E>
where
    F: AddAssign<u128> + UpdateValue<u128> + DefaultFromExtra<E>,
{
    /// Updates and overwrite the fee counter into the specific
    /// value provided.
    ///
    /// IMPORTANT: This function does not affect the buffer window fee
    pub fn update(&mut self, id: Address, value: u128, count: u64) {
        if !value.should_remove() {
            // insert or update, if update remove old fee from total
            let fee = self
                .id_to_fee
                .entry(id)
                .or_insert(F::default_from_extra(&self.extra_data));
            self.total_fee -= fee.get_fee();
            fee.update(value, count);
            self.total_fee += value;
        } else if let Some(old_fee) = self.id_to_fee.remove(&id) {
            self.total_fee -= old_fee.get_fee();
        }
    }

    pub fn get_heaviest_allocation_id(&mut self) -> Option<Address> {
        // just loop over and get the biggest fee
        self.id_to_fee
            .iter_mut()
            .filter(|(_, fee)| !fee.should_filter_out())
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
        self.total_fee - self.fees_requesting
    }
}

impl<E> GenericTracker<BufferedReceiptFee, E> {
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
}

impl GenericTracker<BufferedReceiptFee, DurationInfo> {
    pub fn new(buffer_duration: Duration) -> Self {
        Self {
            extra_data: DurationInfo { buffer_duration },
            total_fee: 0,
            fees_requesting: 0,
            id_to_fee: Default::default(),
        }
    }

    /// Adds into the total_fee entry and buffer window totals
    ///
    /// It's important to notice that `value` cannot be less than
    /// zero, so the only way to make this counter lower is by using
    /// `update` function
    pub fn add(&mut self, id: Address, value: u128) {
        let entry = self
            .id_to_fee
            .entry(id)
            .or_insert(BufferedReceiptFee::default_from_extra(&self.extra_data));
        self.total_fee += value;
        *entry += value;
        entry.count += 1;
        if self.extra_data.buffer_duration > Duration::ZERO {
            let now = Instant::now();
            entry.entries.push_back((now, value));
            entry.fee_in_buffer += value;
        }
    }

    pub fn get_total_fee_outside_buffer(&mut self) -> u128 {
        self.get_total_fee() - self.get_buffer_fee().min(self.total_fee)
    }

    pub fn get_total_counter_outside_buffer_for_allocation(
        &mut self,
        allocation_id: &Address,
    ) -> u64 {
        let Some(allocation_info) = self.id_to_fee.get_mut(allocation_id) else {
            return 0;
        };
        let allocation_counter = allocation_info.count;
        let counter_in_buffer = allocation_info.get_count();
        allocation_counter - counter_in_buffer
    }

    pub fn get_buffer_fee(&mut self) -> u128 {
        self.id_to_fee
            .values_mut()
            .fold(0u128, |acc, expiring| acc + expiring.get_sum())
    }

    pub fn start_rav_request(&mut self, allocation_id: Address) {
        let current_fee = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(BufferedReceiptFee::default_from_extra(&self.extra_data));
        current_fee.requesting = true;
        self.fees_requesting += current_fee.total_fee;
    }

    /// Should be called before `update`
    pub fn finish_rav_request(&mut self, allocation_id: Address) {
        let current_fee = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(BufferedReceiptFee::default_from_extra(&self.extra_data));
        current_fee.requesting = false;
        self.fees_requesting -= current_fee.total_fee;
    }

    pub fn failed_rav_backoff(&mut self, allocation_id: Address) {
        // backoff = max(100ms * 2 ^ retries, 60s)
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(BufferedReceiptFee::default_from_extra(&self.extra_data));
        let failed_rav = &mut entry.failed_info;
        failed_rav.failed_rav_backoff_time = Instant::now()
            + (Duration::from_millis(100) * 2u32.pow(failed_rav.failed_ravs_count))
                .min(Duration::from_secs(60));
        failed_rav.failed_ravs_count += 1;
    }
    pub fn ok_rav_request(&mut self, allocation_id: Address) {
        let entry = self
            .id_to_fee
            .entry(allocation_id)
            .or_insert(BufferedReceiptFee::default_from_extra(&self.extra_data));
        entry.failed_info.failed_ravs_count = 0;
    }

    pub fn check_allocation_has_rav_request_running(&self, allocation_id: Address) -> bool {
        self.id_to_fee
            .get(&allocation_id)
            .map(|alloc| alloc.requesting)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use crate::agent::sender_fee_tracker::BufferedReceiptFee;

    use super::SenderFeeTracker;
    use alloy::primitives::address;
    use std::{thread::sleep, time::Duration};

    #[test]
    fn test_allocation_id_tracker() {
        let allocation_id_0 = address!("abababababababababababababababababababab");
        let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
        let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

        let mut tracker = SenderFeeTracker::<BufferedReceiptFee>::default();
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.update(allocation_id_0, 10, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.block_allocation_id(allocation_id_0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.unblock_allocation_id(allocation_id_0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));

        tracker.update(allocation_id_2, 20, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.block_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.unblock_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

        tracker.update(allocation_id_1, 30, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 60);

        tracker.update(allocation_id_2, 10, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.update(allocation_id_2, 40, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 80);

        tracker.update(allocation_id_1, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.update(allocation_id_2, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.update(allocation_id_0, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);
    }

    #[test]
    fn test_buffer_tracker_window() {
        let allocation_id_0 = address!("abababababababababababababababababababab");
        let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
        let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

        const BUFFER_WINDOW: Duration = Duration::from_millis(20);
        let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.add(allocation_id_0, 10);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(tracker.get_total_fee(), 10);

        sleep(BUFFER_WINDOW);

        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.add(allocation_id_2, 20);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
        assert_eq!(tracker.get_total_fee(), 30);

        sleep(BUFFER_WINDOW);

        tracker.block_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 30);
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.unblock_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

        tracker.add(allocation_id_1, 30);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 30);
        assert_eq!(tracker.get_total_fee(), 60);

        sleep(BUFFER_WINDOW);

        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 60);
        assert_eq!(tracker.get_total_fee(), 60);

        tracker.add(allocation_id_2, 20);
        // we just removed, the buffer should have been removed as well
        tracker.update(allocation_id_2, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 40);
        assert_eq!(tracker.get_total_fee(), 40);

        sleep(BUFFER_WINDOW);

        tracker.add(allocation_id_2, 200);
        tracker.update(allocation_id_2, 100, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(tracker.get_total_fee(), 140);

        sleep(BUFFER_WINDOW);

        tracker.update(allocation_id_2, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 40);
        assert_eq!(tracker.get_total_fee(), 40);

        tracker.update(allocation_id_1, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.update(allocation_id_0, 0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(tracker.get_total_fee(), 0);
    }

    #[test]
    fn test_filtered_backed_off_allocations() {
        let allocation_id_0 = address!("abababababababababababababababababababab");
        let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
        const BACK_SLEEP_DURATION: Duration = Duration::from_millis(201);

        let mut tracker = SenderFeeTracker::default();
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.update(allocation_id_0, 10, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.update(allocation_id_1, 20, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 30);

        // Simulate failed rav and backoff
        tracker.failed_rav_backoff(allocation_id_1);

        // Heaviest should be the first since its not blocked nor failed
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 30);

        sleep(BACK_SLEEP_DURATION);

        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 30);
    }

    #[test]
    fn test_ongoing_rav_requests() {
        let allocation_id_0 = address!("abababababababababababababababababababab");
        let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
        let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

        let mut tracker = SenderFeeTracker::default();
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.add(allocation_id_0, 10);
        tracker.add(allocation_id_1, 20);
        tracker.add(allocation_id_2, 30);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 60);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 60);

        tracker.start_rav_request(allocation_id_2);

        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 30);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 30);

        tracker.finish_rav_request(allocation_id_2);

        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 60);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 60);
    }

    #[test]
    fn check_counter_and_fee_outside_buffer_unordered() {
        let allocation_id_0 = address!("abababababababababababababababababababab");
        let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

        const BUFFER_WINDOW: Duration = Duration::from_millis(20);
        let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(
            tracker.get_total_counter_outside_buffer_for_allocation(&allocation_id_0),
            0
        );

        tracker.add(allocation_id_0, 10);
        assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
        assert_eq!(
            tracker.get_total_counter_outside_buffer_for_allocation(&allocation_id_0),
            0
        );

        sleep(BUFFER_WINDOW);

        assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
        assert_eq!(
            tracker.get_total_counter_outside_buffer_for_allocation(&allocation_id_0),
            1
        );

        tracker.add(allocation_id_2, 20);
        assert_eq!(
            tracker.get_total_counter_outside_buffer_for_allocation(&allocation_id_2),
            0
        );
        assert_eq!(tracker.get_total_fee_outside_buffer(), 10);

        sleep(BUFFER_WINDOW);

        tracker.block_allocation_id(allocation_id_2);
        assert_eq!(
            tracker.get_total_counter_outside_buffer_for_allocation(&allocation_id_2),
            1
        );
        assert_eq!(tracker.get_total_fee_outside_buffer(), 30);
    }

    #[test]
    fn check_get_count_updates_sum() {
        let allocation_id_0 = address!("abababababababababababababababababababab");

        const BUFFER_WINDOW: Duration = Duration::from_millis(20);
        let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);

        tracker.add(allocation_id_0, 10);
        let expiring_sum = tracker
            .id_to_fee
            .get_mut(&allocation_id_0)
            .expect("there should be something here");
        assert_eq!(expiring_sum.get_sum(), 10);
        assert_eq!(expiring_sum.get_count(), 1);

        sleep(BUFFER_WINDOW);

        assert_eq!(expiring_sum.get_sum(), 0);
        assert_eq!(expiring_sum.get_count(), 0);

        tracker.add(allocation_id_0, 10);
        let expiring_sum = tracker
            .id_to_fee
            .get_mut(&allocation_id_0)
            .expect("there should be something here");

        assert_eq!(expiring_sum.get_count(), 1);
        assert_eq!(expiring_sum.get_sum(), 10);

        sleep(BUFFER_WINDOW);

        assert_eq!(expiring_sum.get_count(), 0);
        assert_eq!(expiring_sum.get_sum(), 0);
    }
}
