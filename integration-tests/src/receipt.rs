// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use rand::{rng, Rng};
use std::str::FromStr;
use std::time::SystemTime;

use anyhow::Result;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::Receipt;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

pub fn create_tap_receipt(
    value: u128,
    allocation_id: &Address,
    escrow_contract: &str,
    wallet: &PrivateKeySigner,
) -> Result<Eip712SignedMessage<Receipt>> {
    let nonce = rng().random::<u64>();

    // Get timestamp in nanoseconds
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let timestamp_ns = timestamp as u64;

    // Create domain separator
    let eip712_domain_separator = tap_eip712_domain(
        1337,                                // chain_id
        Address::from_str(escrow_contract)?, // verifying_contract
    );

    // Create and sign receipt
    println!("Creating and signing receipt...");
    let receipt = Eip712SignedMessage::new(
        &eip712_domain_separator,
        Receipt {
            allocation_id: *allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        wallet,
    )?;

    Ok(receipt)
}
