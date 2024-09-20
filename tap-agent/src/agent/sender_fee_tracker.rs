// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::Address;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use tracing::error;

#[derive(Debug, Clone, Default)]
struct ExpiringSum {
    entries: VecDeque<(Instant, u128)>,
    sum: u128,
}

impl ExpiringSum {
    fn get_sum(&mut self, duration: &Duration) -> u128 {
        let now = Instant::now();
        while let Some(&(timestamp, value)) = self.entries.front() {
            if now.duration_since(timestamp) >= *duration {
                self.entries.pop_front();
                self.sum -= value;
            } else {
                break;
            }
        }
        self.sum
    }
}

#[derive(Debug, Clone, Default)]
pub struct SenderFeeTracker {
    id_to_fee: HashMap<Address, u128>,
    total_fee: u128,

    buffer_fee: HashMap<Address, ExpiringSum>,

    buffer_duration: Duration,
    // there are some allocations that we don't want it to be
    // heaviest allocation, because they are already marked for finalization,
    // and thus requesting RAVs on their own in their `post_stop` routine.
    blocked_addresses: HashSet<Address>,
}

impl SenderFeeTracker {
    pub fn new(buffer_duration: Duration) -> Self {
        Self {
            buffer_duration,
            ..Default::default()
        }
    }
    pub fn add(&mut self, id: Address, value: u128) {
        if self.buffer_duration > Duration::ZERO {
            let now = Instant::now();
            let expiring_sum = self.buffer_fee.entry(id).or_default();
            expiring_sum.entries.push_back((now, value));
            expiring_sum.sum += value;
        }
        self.total_fee += value;
        let entry = self.id_to_fee.entry(id).or_default();
        *entry += value;
    }

    pub fn update(&mut self, id: Address, fee: u128) {
        if fee > 0 {
            // insert or update, if update remove old fee from total
            if let Some(old_fee) = self.id_to_fee.insert(id, fee) {
                self.total_fee -= old_fee;
            }
            self.total_fee = self.total_fee.checked_add(fee).unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                error!(
                    "Overflow when adding receipt value {} to total fee {}. \
                        Setting total fee to u128::MAX.",
                    fee, self.total_fee
                );
                u128::MAX
            });
        } else if let Some(old_fee) = self.id_to_fee.remove(&id) {
            self.total_fee -= old_fee;
        }
    }

    pub fn block_allocation_id(&mut self, address: Address) {
        self.blocked_addresses.insert(address);
    }

    pub fn unblock_allocation_id(&mut self, address: Address) {
        self.blocked_addresses.remove(&address);
    }

    pub fn get_heaviest_allocation_id(&mut self) -> Option<Address> {
        // just loop over and get the biggest fee
        self.id_to_fee
            .iter()
            .filter(|(addr, _)| !self.blocked_addresses.contains(*addr))
            // map to the value minus fees in buffer
            .map(|(addr, fee)| {
                (
                    addr,
                    fee - self
                        .buffer_fee
                        .get_mut(addr)
                        .map(|expiring| expiring.get_sum(&self.buffer_duration))
                        .unwrap_or_default(),
                )
            })
            .fold(None, |acc: Option<(&Address, u128)>, (addr, fee)| {
                if let Some((_, max_fee)) = acc {
                    if fee > max_fee {
                        Some((addr, fee))
                    } else {
                        acc
                    }
                } else {
                    Some((addr, fee))
                }
            })
            .map(|(&id, _)| id)
    }

    pub fn get_list_of_allocation_ids(&self) -> HashSet<Address> {
        self.id_to_fee.keys().cloned().collect()
    }

    pub fn get_total_fee(&self) -> u128 {
        self.total_fee
    }

    pub fn get_total_fee_outsite_buffer(&mut self) -> u128 {
        self.total_fee - self.get_buffer_fee()
    }

    pub fn get_buffer_fee(&mut self) -> u128 {
        self.buffer_fee.values_mut().fold(0u128, |acc, expiring| {
            acc + expiring.get_sum(&self.buffer_duration)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SenderFeeTracker;
    use std::str::FromStr;
    use thegraph_core::Address;

    #[test]
    fn test_allocation_id_tracker() {
        let allocation_id_0: Address =
            Address::from_str("0xabababababababababababababababababababab").unwrap();
        let allocation_id_1: Address =
            Address::from_str("0xbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc").unwrap();
        let allocation_id_2: Address =
            Address::from_str("0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd").unwrap();

        let mut tracker = SenderFeeTracker::default();
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.update(allocation_id_0, 10);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.block_allocation_id(allocation_id_0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.unblock_allocation_id(allocation_id_0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));

        tracker.update(allocation_id_2, 20);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.block_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.unblock_allocation_id(allocation_id_2);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

        tracker.update(allocation_id_1, 30);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 60);

        tracker.update(allocation_id_2, 10);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.update(allocation_id_2, 40);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 80);

        tracker.update(allocation_id_1, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.update(allocation_id_2, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.update(allocation_id_0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);
    }
}
