// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_sol_types::{eip712_domain, Eip712Domain};
use lazy_static::lazy_static;

use crate::config::Config;

lazy_static! {
    pub static ref CONFIG: Config = Config::from_cli().expect("Failed to load configuration");
    pub static ref EIP_712_DOMAIN: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: CONFIG.receipts.receipts_verifier_chain_id,
        verifying_contract: CONFIG.receipts.receipts_verifier_address,
    };
}

pub mod agent;
pub mod aggregator_endpoints;
pub mod config;
pub mod database;
pub mod metrics;
pub mod tap;
