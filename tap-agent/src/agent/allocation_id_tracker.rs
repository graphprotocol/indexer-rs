// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Default)]
pub struct AllocationIdTracker {
    id_to_fee: HashMap<Address, u128>,
    fee_to_count: BTreeMap<u128, u32>,
    total_fee: u128,
}

impl AllocationIdTracker {
    pub fn add_or_update(&mut self, id: Address, fee: u128) {
        if let Some(&old_fee) = self.id_to_fee.get(&id) {
            self.total_fee -= old_fee;
            *self.fee_to_count.get_mut(&old_fee).unwrap() -= 1;
            if self.fee_to_count[&old_fee] == 0 {
                self.fee_to_count.remove(&old_fee);
            }
        }

        if fee > 0 {
            self.id_to_fee.insert(id, fee);
            self.total_fee += fee;
            *self.fee_to_count.entry(fee).or_insert(0) += 1;
        } else {
            self.id_to_fee.remove(&id);
        }
    }

    pub fn get_heaviest_allocation_id(&self) -> Option<Address> {
        self.fee_to_count.iter().next_back().and_then(|(&fee, _)| {
            self.id_to_fee
                .iter()
                .find(|(_, &f)| f == fee)
                .map(|(&id, _)| id)
        })
    }

    pub fn get_total_fee(&self) -> u128 {
        self.total_fee
    }
}

#[cfg(test)]
mod tests {
    use super::AllocationIdTracker;
    use std::str::FromStr;
    use thegraph::types::Address;

    #[test]
    fn test_allocation_id_tracker() {
        let allocation_id_0: Address =
            Address::from_str("0xabababababababababababababababababababab").unwrap();
        let allocation_id_1: Address =
            Address::from_str("0xbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc").unwrap();
        let allocation_id_2: Address =
            Address::from_str("0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd").unwrap();

        let mut tracker = AllocationIdTracker::default();
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);

        tracker.add_or_update(allocation_id_0, 10);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.add_or_update(allocation_id_2, 20);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 30);

        tracker.add_or_update(allocation_id_1, 30);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 60);

        tracker.add_or_update(allocation_id_2, 10);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.add_or_update(allocation_id_2, 40);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 80);

        tracker.add_or_update(allocation_id_1, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
        assert_eq!(tracker.get_total_fee(), 50);

        tracker.add_or_update(allocation_id_2, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
        assert_eq!(tracker.get_total_fee(), 10);

        tracker.add_or_update(allocation_id_0, 0);
        assert_eq!(tracker.get_heaviest_allocation_id(), None);
        assert_eq!(tracker.get_total_fee(), 0);
    }
}
