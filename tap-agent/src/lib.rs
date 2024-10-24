// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::dyn_abi::Eip712Domain;
use lazy_static::lazy_static;
use tap_core::tap_eip712_domain;

use crate::config::Config;

lazy_static! {
    pub static ref CONFIG: Config = Config::from_cli().expect("Failed to load configuration");
    pub static ref EIP_712_DOMAIN: Eip712Domain = tap_eip712_domain(
        CONFIG.receipts.receipts_verifier_chain_id,
        CONFIG.receipts.receipts_verifier_address,
    );
}

pub mod adaptative_concurrency;
pub mod agent;
pub mod backoff;
pub mod config;
pub mod database;
pub mod metrics;
pub mod tap;
pub mod tracker;
