// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#![warn(missing_docs)] // Warns if any public item lacks documentation

//! # Tap-Agent
//!
//! This software is used to monitor receipts inserted in the database
//! and to keep track of all the values across different allocations.
//!
//! Its main goal is that the value never goes below the balance available
//! in the escrow account for a given sender.

use std::sync::LazyLock;

use indexer_config::Config;
use tap_core::tap_eip712_domain;
use thegraph_core::alloy::sol_types::Eip712Domain;

/// Static configuration
pub static CONFIG: LazyLock<Config> =
    LazyLock::new(|| cli::get_config().expect("Failed to load configuration"));

/// Static EIP_712_DOMAIN used with config values for V2(Horizon)
pub static EIP_712_DOMAIN_V2: LazyLock<Eip712Domain> = LazyLock::new(|| {
    tap_eip712_domain(
        CONFIG.blockchain.chain_id as u64,
        if CONFIG.horizon.enabled {
            CONFIG
                .blockchain
                .receipts_verifier_address_v2
                .expect("receipts_verifier_address_v2 is required when Horizon is enabled")
        } else {
            CONFIG
                .blockchain
                .receipts_verifier_address_v2
                .unwrap_or(CONFIG.blockchain.receipts_verifier_address)
        },
        tap_core::TapVersion::V2,
    )
});

pub mod adaptative_concurrency;
pub mod agent;
pub mod backoff;
pub mod cli;
/// Database helper
pub mod database;
/// Prometheus Metrics server
pub mod metrics;
pub mod tap;

/// Test utils to interact with Tap Actors
#[cfg(any(test, feature = "test"))]
pub mod test;
pub mod tracker;
