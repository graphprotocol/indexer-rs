use super::{AllocationStats, DefaultFromExtra, DurationInfo, SenderFeeStats};
use alloy::primitives::Address;
use std::{
    collections::{HashMap, HashSet},
    ops::AddAssign,
    time::{Duration, Instant},
};

pub trait GlobalTracker {
    fn should_remove(&self) -> bool;
}

impl GlobalTracker for u128 {
    fn should_remove(&self) -> bool {
        *self == 0
    }
}

type GlobalFeeTracker = u128;

#[derive(Debug, Clone, Default)]
pub struct GenericTracker<F, E> {
    pub(super) id_to_fee: HashMap<Address, F>,
    pub(super) total_fee: u128,
    pub(super) extra_data: E,
}

impl<F, E> GenericTracker<F, E>
where
    F: AddAssign<GlobalFeeTracker> + AllocationStats<GlobalFeeTracker> + DefaultFromExtra<E>,
{
    /// Updates and overwrite the fee counter into the specific
    /// value provided.
    ///
    /// IMPORTANT: This function does not affect the buffer window fee
    pub fn update(&mut self, id: Address, value: u128) {
        if !value.should_remove() {
            // insert or update, if update remove old fee from total
            let fee = self
                .id_to_fee
                .entry(id)
                .or_insert(F::default_from_extra(&self.extra_data));
            self.total_fee -= fee.get_fee();
            fee.update(value);
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
        self.total_fee
    }
}

impl GenericTracker<SenderFeeStats, DurationInfo> {
    pub fn new(buffer_duration: Duration) -> Self {
        Self {
            extra_data: DurationInfo { buffer_duration },
            total_fee: 0,
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
            .or_insert(SenderFeeStats::default_from_extra(&self.extra_data));
        self.total_fee += value;
        *entry += value;
        if self.extra_data.buffer_duration > Duration::ZERO {
            let now = Instant::now();
            entry.entries.push_back((now, value));
            entry.fee_in_buffer += value;
        }
    }

    pub fn get_total_fee_outside_buffer(&mut self) -> u128 {
        self.total_fee - self.get_buffer_fee().min(self.total_fee)
    }

    pub fn get_buffer_fee(&mut self) -> u128 {
        self.id_to_fee
            .values_mut()
            .fold(0u128, |acc, expiring| acc + expiring.get_sum())
    }
}

impl<E> GenericTracker<SenderFeeStats, E> {
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

impl AllocationStats<GlobalFeeTracker> for u128 {
    fn update(&mut self, v: u128) {
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
