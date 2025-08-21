// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Test to verify V2 signature creation and recovery works correctly

use anyhow::Result;
use std::str::FromStr;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::v2::Receipt as V2Receipt;
use thegraph_core::{
    alloy::{primitives::Address, signers::local::PrivateKeySigner},
    CollectionId,
};

use crate::constants::{
    ACCOUNT0_SECRET, CHAIN_ID, GRAPH_TALLY_COLLECTOR_CONTRACT, TEST_DATA_SERVICE,
};

pub async fn test_v2_signature_recovery() -> Result<()> {
    println!("=== V2 Signature Recovery Test ===");

    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse()?;
    let wallet_address = wallet.address();
    println!("Wallet address: {wallet_address:?}");

    // Create EIP-712 domain - V2 uses GraphTally
    let domain = tap_eip712_domain(
        CHAIN_ID,
        Address::from_str(GRAPH_TALLY_COLLECTOR_CONTRACT)?,
        tap_core::TapVersion::V2,
    );
    println!("Using domain: chain_id={CHAIN_ID}, verifier={GRAPH_TALLY_COLLECTOR_CONTRACT}");

    // Create a V2 receipt
    let allocation_id = Address::from_str("0xc172ed1c6470dfa3b12a789317dda50cdd8b85df")?;
    let collection_id = CollectionId::from(allocation_id);
    let payer = wallet_address;
    let service_provider = allocation_id;
    let data_service = Address::from_str(TEST_DATA_SERVICE)?;

    println!("V2 Receipt parameters:");
    println!("  Collection ID: {collection_id:?}");
    println!("  Payer: {payer:?}");
    println!("  Service provider: {service_provider:?}");
    println!("  Data service: {data_service:?}");

    let receipt = V2Receipt::new(
        *collection_id,
        payer,
        data_service,
        service_provider,
        100_000_000_000_000_000u128, // 0.1 GRT
    )?;

    // Sign the receipt
    let signed_receipt = Eip712SignedMessage::new(&domain, receipt, &wallet)?;
    println!("Receipt signed successfully");

    // Recover the signer
    let recovered_signer = signed_receipt.recover_signer(&domain)?;
    println!("Recovered signer: {recovered_signer:?}");
    println!("Expected signer: {wallet_address:?}");

    // Check if they match
    if recovered_signer == wallet_address {
        println!("✅ SUCCESS: Signature recovery matches wallet address");
        Ok(())
    } else {
        println!("❌ FAILURE: Signature recovery mismatch!");
        println!("  Expected: {wallet_address:?}");
        println!("  Got:      {recovered_signer:?}");
        Err(anyhow::anyhow!("Signature recovery failed for V2 receipt"))
    }
}
