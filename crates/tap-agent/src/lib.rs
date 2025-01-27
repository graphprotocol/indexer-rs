// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_config::Config;
use lazy_static::lazy_static;
use tap_core::tap_eip712_domain;
use thegraph_core::alloy::sol_types::Eip712Domain;

lazy_static! {
    pub static ref CONFIG: Config = cli::get_config().expect("Failed to load configuration");
    pub static ref EIP_712_DOMAIN: Eip712Domain = tap_eip712_domain(
        CONFIG.blockchain.chain_id as u64,
        CONFIG.blockchain.receipts_verifier_address,
    );
}

pub mod adaptative_concurrency;
pub mod agent;
pub mod backoff;
pub mod cli;
pub mod database;
pub mod metrics;
pub mod tap;
#[cfg(any(test, feature = "test"))]
pub mod test;
pub mod tracker;
