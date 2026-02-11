// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Minimum price enforcement for RCA proposals.
//!
//! Indexers configure minimum acceptable prices for their services. This module
//! validates that RCA proposals meet these minimums before acceptance.
//!
//! # Pricing Model
//!
//! RCAs specify two pricing components:
//!
//! - **tokens_per_second** - Base rate for the indexing service, per network
//! - **tokens_per_entity_per_second** - Additional rate based on indexed entities
//!
//! Both values are in wei GRT (10^-18 GRT). The indexer configures minimum
//! acceptable values; proposals offering less are rejected.
//!
//! # Per-Network Pricing
//!
//! Different networks have different operational costs (RPC fees, storage, etc.).
//! The `tokens_per_second` minimum is configured per network:
//!
//! ```toml
//! [dips.tokens_per_second]
//! mainnet = "1000000000000"  # Higher cost chain
//! arbitrum-one = "500000000000"  # Lower cost L2
//! ```
//!
//! Networks not in this map are considered unsupported and will be rejected.

use std::collections::BTreeMap;

use thegraph_core::alloy::primitives::U256;

#[derive(Debug, Default)]
pub struct PriceCalculator {
    tokens_per_second: BTreeMap<String, U256>,
    tokens_per_entity_per_second: U256,
}

impl PriceCalculator {
    pub fn new(
        tokens_per_second: BTreeMap<String, U256>,
        tokens_per_entity_per_second: U256,
    ) -> Self {
        Self {
            tokens_per_second,
            tokens_per_entity_per_second,
        }
    }

    #[cfg(test)]
    pub fn for_testing() -> Self {
        Self {
            tokens_per_second: BTreeMap::from_iter(vec![("mainnet".to_string(), U256::from(200))]),
            tokens_per_entity_per_second: U256::from(100),
        }
    }

    pub fn is_supported(&self, network: &str) -> bool {
        self.get_minimum_price(network).is_some()
    }

    pub fn get_minimum_price(&self, network: &str) -> Option<U256> {
        self.tokens_per_second.get(network).copied()
    }

    pub fn entity_price(&self) -> U256 {
        self.tokens_per_entity_per_second
    }
}
