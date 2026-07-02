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
//! The `tokens_per_second` minimum is configured per network.
//!
//! Networks must also be in `supported_networks` to accept proposals.

use std::collections::{BTreeMap, HashSet};

use thegraph_core::alloy::primitives::U256;

#[derive(Debug, Default)]
pub struct PriceCalculator {
    supported_networks: HashSet<String>,
    tokens_per_second: BTreeMap<String, U256>,
    tokens_per_entity_per_second: U256,
}

impl PriceCalculator {
    pub fn new(
        supported_networks: HashSet<String>,
        tokens_per_second: BTreeMap<String, U256>,
        tokens_per_entity_per_second: U256,
    ) -> Self {
        Self {
            supported_networks,
            tokens_per_second,
            tokens_per_entity_per_second,
        }
    }

    #[cfg(test)]
    pub fn for_testing() -> Self {
        Self {
            supported_networks: HashSet::from(["mainnet".to_string()]),
            tokens_per_second: BTreeMap::from_iter(vec![("mainnet".to_string(), U256::from(200))]),
            tokens_per_entity_per_second: U256::from(100),
        }
    }

    /// Check if a network is supported.
    ///
    /// A network is supported if:
    /// 1. It's in the explicit `supported_networks` list, AND
    /// 2. It has pricing configured
    pub fn is_supported(&self, network: &str) -> bool {
        self.supported_networks.contains(network) && self.tokens_per_second.contains_key(network)
    }

    pub fn get_minimum_price(&self, network: &str) -> Option<U256> {
        if !self.supported_networks.contains(network) {
            return None;
        }
        self.tokens_per_second.get(network).copied()
    }

    pub fn entity_price(&self) -> U256 {
        self.tokens_per_entity_per_second
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_minimum_price_existing_network() {
        // Arrange
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string()]),
            BTreeMap::from([("mainnet".to_string(), U256::from(1000))]),
            U256::from(50),
        );

        // Act
        let price = calculator.get_minimum_price("mainnet");

        // Assert
        assert_eq!(price, Some(U256::from(1000)));
    }

    #[test]
    fn test_get_minimum_price_missing_network() {
        // Arrange
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string()]),
            BTreeMap::from([("mainnet".to_string(), U256::from(1000))]),
            U256::from(50),
        );

        // Act
        let price = calculator.get_minimum_price("arbitrum-one");

        // Assert
        assert_eq!(price, None);
    }

    #[test]
    fn test_is_supported_true() {
        // Arrange
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string(), "arbitrum-one".to_string()]),
            BTreeMap::from([
                ("mainnet".to_string(), U256::from(1000)),
                ("arbitrum-one".to_string(), U256::from(500)),
            ]),
            U256::from(50),
        );

        // Act & Assert
        assert!(calculator.is_supported("mainnet"));
        assert!(calculator.is_supported("arbitrum-one"));
    }

    #[test]
    fn test_is_supported_false_not_in_list() {
        // Arrange - network has pricing but not in supported list
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string()]),
            BTreeMap::from([
                ("mainnet".to_string(), U256::from(1000)),
                ("arbitrum-one".to_string(), U256::from(500)),
            ]),
            U256::from(50),
        );

        // Act & Assert
        assert!(calculator.is_supported("mainnet"));
        assert!(!calculator.is_supported("arbitrum-one")); // Has pricing but not in supported list
    }

    #[test]
    fn test_is_supported_false_no_pricing() {
        // Arrange - network in supported list but no pricing
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string(), "arbitrum-one".to_string()]),
            BTreeMap::from([("mainnet".to_string(), U256::from(1000))]),
            U256::from(50),
        );

        // Act & Assert
        assert!(calculator.is_supported("mainnet"));
        assert!(!calculator.is_supported("arbitrum-one")); // In list but no pricing
    }

    #[test]
    fn test_is_supported_false_unknown() {
        // Arrange
        let calculator = PriceCalculator::new(
            HashSet::from(["mainnet".to_string()]),
            BTreeMap::from([("mainnet".to_string(), U256::from(1000))]),
            U256::from(50),
        );

        // Act & Assert
        assert!(!calculator.is_supported("optimism"));
        assert!(!calculator.is_supported(""));
    }

    #[test]
    fn test_entity_price() {
        // Arrange
        let calculator = PriceCalculator::new(HashSet::new(), BTreeMap::new(), U256::from(12345));

        // Act
        let price = calculator.entity_price();

        // Assert
        assert_eq!(price, U256::from(12345));
    }

    #[test]
    fn test_empty_config() {
        // Arrange
        let calculator = PriceCalculator::new(HashSet::new(), BTreeMap::new(), U256::from(100));

        // Act & Assert
        assert!(!calculator.is_supported("mainnet"));
        assert_eq!(calculator.get_minimum_price("mainnet"), None);
    }

    #[test]
    fn test_default() {
        // Arrange & Act
        let calculator = PriceCalculator::default();

        // Assert
        assert!(!calculator.is_supported("mainnet"));
        assert_eq!(calculator.entity_price(), U256::ZERO);
    }
}
