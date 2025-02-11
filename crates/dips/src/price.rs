// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct PriceCalculator {
    prices_per_chain: HashMap<String, u64>,
    default_price: Option<u64>,
}

impl PriceCalculator {
    #[cfg(test)]
    pub fn for_testing() -> Self {
        Self {
            prices_per_chain: HashMap::default(),
            default_price: Some(100),
        }
    }

    pub fn is_supported(&self, chain_id: &str) -> bool {
        self.get_minimum_price(chain_id).is_some()
    }
    pub fn get_minimum_price(&self, chain_id: &str) -> Option<u64> {
        let chain_price = self.prices_per_chain.get(chain_id).copied();

        chain_price.or(self.default_price)
    }
}
