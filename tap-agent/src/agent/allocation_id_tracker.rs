// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct AllocationIdTracker {
    id_to_fee: HashMap<Address, u128>,
    fee_to_count: BTreeMap<u128, u32>,
    total_fee: u128,
}

impl AllocationIdTracker {
    pub fn new() -> Self {
        AllocationIdTracker {
            id_to_fee: HashMap::new(),
            fee_to_count: BTreeMap::new(),
            total_fee: 0,
        }
    }

    pub fn add_or_update(&mut self, id: Address, fee: u128) {
        if let Some(&old_fee) = self.id_to_fee.get(&id) {
            self.total_fee -= old_fee;
            *self.fee_to_count.get_mut(&old_fee).unwrap() -= 1;
            if self.fee_to_count[&old_fee] == 0 {
                self.fee_to_count.remove(&old_fee);
            }
        }

        self.id_to_fee.insert(id, fee);
        self.total_fee += fee;
        *self.fee_to_count.entry(fee).or_insert(0) += 1;
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
