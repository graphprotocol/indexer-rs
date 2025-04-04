// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thegraph_core::alloy::signers::local::coins_bip39::English;
use thegraph_core::alloy::{
    primitives::Address,
    signers::local::{MnemonicBuilder, PrivateKeySigner},
};

mod metrics;
mod receipt;
use metrics::MetricsChecker;
use receipt::create_tap_receipt;

// TODO: Would be nice to read this values from:
// contrib/tap-agent/config.toml
// and contrib/local-network/.env
const GATEWAY_URL: &str = "http://localhost:7700";
const SUBGRAPH_ID: &str = "BFr2mx7FgkJ36Y6pE5BiXs1KmNUmVDCnL82KUSdcLW1g";
const TAP_ESCROW_CONTRACT: &str = "0x0355B7B8cb128fA5692729Ab3AAa199C1753f726";
const GATEWAY_API_KEY: &str = "deadbeefdeadbeefdeadbeefdeadbeef";
// const RECEIVER_ADDRESS: &str = "0xf4EF6650E48d099a4972ea5B414daB86e1998Bd3";
const TAP_AGENT_METRICS_URL: &str = "http://localhost:7300/metrics";

const MNEMONIC: &str = "test test test test test test test test test test test junk";
const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

const GRT_DECIMALS: u8 = 18;
const GRT_BASE: u128 = 10u128.pow(GRT_DECIMALS as u32);

// With trigger_value_divisor = 500_000 and max_amount_willing_to_lose_grt = 1000
// trigger_value = 0.002 GRT
// We need to send at least 20 receipts to reach the trigger threshold
// Sending slightly more than required to ensure triggering
const MAX_RECEIPT_VALUE: u128 = GRT_BASE / 1_000;
// This value should match the timestamp_buffer_secs
// in the tap-agent setting + 10 seconds
const WAIT_TIME_BATCHES: u64 = 40;

// Bellow constant is to define the number of receipts
const NUM_RECEIPTS: u32 = 200;

// Send receipts in batches with a delay in between
// to ensure some receipts get outside the timestamp buffer
const BATCHES: u32 = 4;

#[tokio::main]
async fn main() -> Result<()> {
    // Run the TAP receipt test
    tap_rav_test().await
}

async fn tap_rav_test() -> Result<()> {
    // Setup wallet using your MnemonicBuilder
    let index: u32 = 0;
    let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
        .phrase(MNEMONIC)
        .index(index)
        .unwrap()
        .build()
        .unwrap();

    let sender_address = wallet.address();
    println!("Using sender address: {}", sender_address);

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Query the network subgraph to find active allocations
    println!("Querying for active allocations...");
    let response = http_client
        .post(GRAPH_URL)
        .json(&json!({
            "query": "{ allocations(where: { status: Active }) { id indexer { id } subgraphDeployment { id } } }"
        }))
        .send()
        .await?;

    // Default to a fallback allocation ID
    let mut allocation_id = Address::from_str("0x0000000000000000000000000000000000000000")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Network subgraph request failed with status: {}",
            response.status()
        ));
    }

    // Try to find a valid allocation
    let response_text = response.text().await?;
    println!("Network subgraph response: {}", response_text);

    // Extract allocation_id, if not found return an error
    // this should not happen as we run the fund escrow script
    // before(in theory tho)
    let json_value = serde_json::from_str::<serde_json::Value>(&response_text)?;
    let allocation_id = json_value
        .get("data")
        .and_then(|d| d.get("allocations"))
        .and_then(|a| a.as_array())
        .filter(|arr| !arr.is_empty())
        .and_then(|arr| arr[0].get("id"))
        .and_then(|id| id.as_str())
        .ok_or_else(|| anyhow::anyhow!("No valid allocation ID found"))?;

    let allocation_id = Address::from_str(allocation_id)?;

    // Create a metrics checker
    let metrics_checker =
        MetricsChecker::new(http_client.clone(), TAP_AGENT_METRICS_URL.to_string());

    let initial_metrics = metrics_checker.get_current_metrics().await?;
    // Extract the initial metrics we care about
    // in this case the number of created/requested ravs
    // and the amount of unaggregated fees
    // so later at the end of the test we compare them
    // to see if we triggered any RAV generation
    let initial_ravs_created =
        initial_metrics.ravs_created_by_allocation(&allocation_id.to_string());
    let initial_unaggregated =
        initial_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

    println!(
        "\n=== Sending {} receipts to trigger RAV generation ===",
        NUM_RECEIPTS
    );
    println!(
        "Each receipt value: {} GRT",
        MAX_RECEIPT_VALUE as f64 / GRT_BASE as f64
    );
    println!(
        "Total value to be sent: {} GRT",
        (MAX_RECEIPT_VALUE as f64 * NUM_RECEIPTS as f64) / GRT_BASE as f64
    );

    let receipts_per_batch = NUM_RECEIPTS / BATCHES;
    let mut total_successful = 0;

    for batch in 0..BATCHES {
        println!(
            "Sending batch {} of {} ({} receipts per batch)",
            batch + 1,
            BATCHES,
            receipts_per_batch
        );

        for i in 0..receipts_per_batch {
            let receipt_index = batch * receipts_per_batch + i;
            println!("Sending receipt {} of {}", receipt_index + 1, NUM_RECEIPTS);

            // Create TAP receipt
            let receipt = create_tap_receipt(
                MAX_RECEIPT_VALUE,
                &allocation_id,
                TAP_ESCROW_CONTRACT,
                &wallet,
            )?;
            let receipt_json = serde_json::to_string(&receipt).unwrap();

            let response = http_client
                .post(format!("{}/api/subgraphs/id/{}", GATEWAY_URL, SUBGRAPH_ID))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", GATEWAY_API_KEY))
                .header("Tap-Receipt", receipt_json)
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            let status = response.status();
            if status.is_success() {
                total_successful += 1;
                println!("Receipt {} sent successfully", receipt_index + 1);
            } else {
                println!("Failed to send receipt {}: {}", receipt_index + 1, status);
                let response_text = response.text().await?;
                println!("Response: {}", response_text);
            }

            // Small delay between queries to avoid flooding
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // After each batch, wait longer than the timestamp buffer
        // (typically 30 seconds) to ensure receipts are outside buffer
        if batch < BATCHES - 1 {
            println!(
                "\nBatch {} complete. Waiting to exceed timestamp buffer...",
                batch + 1
            );
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES * 2)).await;
        }
    }

    println!("\n=== Summary ===");
    println!(
        "Total receipts sent successfully: {}/{}",
        total_successful, NUM_RECEIPTS
    );
    println!(
        "Total value sent: {} GRT",
        (MAX_RECEIPT_VALUE as f64 * total_successful as f64) / GRT_BASE as f64
    );

    // Give the system enough time to process the receipts
    // ensuring the aged beyong timestamp buffer
    tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES * 4)).await;

    // Check for RAV generation
    println!("\n=== Checking for RAV generation ===");

    // Get final metrics
    println!("Getting final metrics snapshot...");
    let final_metrics = metrics_checker.get_current_metrics().await?;

    // Extract the final metrics
    let final_ravs_created = final_metrics.ravs_created_by_allocation(&allocation_id.to_string());
    let final_unaggregated =
        final_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

    println!(
        "Final state: RAVs created: {}, Unaggregated fees: {}",
        final_ravs_created, final_unaggregated
    );

    // Check for success criteria
    if final_ravs_created > initial_ravs_created {
        println!(
            "✅ TEST PASSED: RAVs created increased from {} to {}!",
            initial_ravs_created, final_ravs_created
        );
        return Ok(());
    }

    if final_unaggregated < initial_unaggregated * 0.9 {
        println!(
            "✅ TEST PASSED: Unaggregated fees decreased significantly from {} to {}!",
            initial_unaggregated, final_unaggregated
        );
        return Ok(());
    }

    // If we got here, test failed
    println!("❌ TEST FAILED: No RAV generation detected");

    Err(anyhow::anyhow!("Failed to detect RAV generation"))
}
