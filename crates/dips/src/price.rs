// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct PriceCalculator {
    base_price_per_epoch: BTreeMap<String, u64>,
    price_per_entity: u64,
}

impl PriceCalculator {
    pub fn new(base_price_per_epoch: BTreeMap<String, u64>, price_per_entity: u64) -> Self {
        Self {
            base_price_per_epoch,
            price_per_entity,
        }
    }

    #[cfg(test)]
    pub fn for_testing() -> Self {
        Self {
            base_price_per_epoch: BTreeMap::from_iter(vec![("mainnet".to_string(), 200)]),
            price_per_entity: 100,
        }
    }

    pub fn is_supported(&self, chain_id: &str) -> bool {
        self.get_minimum_price(chain_id).is_some()
    }
    pub fn get_minimum_price(&self, chain_id: &str) -> Option<u64> {
        self.base_price_per_epoch.get(chain_id).copied()
    }

    pub fn entity_price(&self) -> u64 {
        self.price_per_entity
    }
}
