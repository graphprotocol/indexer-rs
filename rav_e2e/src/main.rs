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
const NUM_RECEIPTS: u32 = 70;

// Send receipts in batches with a delay in between
// to ensure some receipts get outside the timestamp buffer
const BATCHES: u32 = 2;
const MAX_TRIGGERS: usize = 100;

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

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Network subgraph request failed with status: {}",
            response.status()
        ));
    }

    // Try to find a valid allocation
    let response_text = response.text().await?;
    println!("Network subgraph response: {}", response_text);

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

    // First check initial metrics
    let initial_metrics = metrics_checker.get_current_metrics().await?;
    let initial_ravs_created =
        initial_metrics.ravs_created_by_allocation(&allocation_id.to_string());
    let initial_unaggregated =
        initial_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

    println!(
        "\n=== Initial metrics: RAVs created: {}, Unaggregated fees: {} ===",
        initial_ravs_created, initial_unaggregated
    );

    // Calculate required receipts to trigger RAV
    // With MAX_RECEIPT_VALUE = GRT_BASE / 1_000 (0.001 GRT)
    // And trigger_value = 0.002 GRT
    // We need at least 3 receipts to trigger a RAV (0.003 GRT > 0.002 GRT)
    const RECEIPTS_NEEDED: u32 = 3;

    println!("\n=== STAGE 1: Sending large receipt batches with small pauses ===");

    // Send multiple receipts in two batches with a gap between them
    let mut total_successful = 0;

    for batch in 0..2 {
        println!(
            "Sending batch {} of 2 with {} receipts each...",
            batch + 1,
            RECEIPTS_NEEDED
        );

        for i in 0..RECEIPTS_NEEDED {
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

            if response.status().is_success() {
                total_successful += 1;
                println!("Receipt {} of batch {} sent successfully", i + 1, batch + 1);
            } else {
                println!("Failed to send receipt: {}", response.status());
            }

            // Small pause between receipts within batch
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Check metrics after batch
        let batch_metrics = metrics_checker.get_current_metrics().await?;
        println!(
            "After batch {}: RAVs created: {}, Unaggregated fees: {}",
            batch + 1,
            batch_metrics.ravs_created_by_allocation(&allocation_id.to_string()),
            batch_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string())
        );

        // Wait between batches - long enough for first batch to exit buffer
        if batch < 1 {
            println!("Waiting for buffer period + 5s...");
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES + 5)).await;
        }
    }

    println!("\n=== STAGE 2: Sending continuous trigger receipts ===");

    // Now send a series of regular receipts with short intervals until RAV is detected
    for i in 0..MAX_TRIGGERS {
        println!("Sending trigger receipt {}/{}...", i + 1, MAX_TRIGGERS);

        let trigger_receipt = create_tap_receipt(
            MAX_RECEIPT_VALUE,
            &allocation_id,
            TAP_ESCROW_CONTRACT,
            &wallet,
        )?;

        let receipt_json = serde_json::to_string(&trigger_receipt).unwrap();

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

        if response.status().is_success() {
            total_successful += 1;
            println!("Trigger receipt {} sent successfully", i + 1);
        } else {
            return Err(anyhow::anyhow!(
                "Failed to send trigger receipt: {}",
                response.status()
            ));
        }

        // Check after each trigger
        tokio::time::sleep(Duration::from_secs(5)).await;

        let current_metrics = metrics_checker.get_current_metrics().await?;
        let current_ravs_created =
            current_metrics.ravs_created_by_allocation(&allocation_id.to_string());
        let current_unaggregated =
            current_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

        println!(
            "After trigger {}: RAVs created: {}, Unaggregated fees: {}",
            i + 1,
            current_ravs_created,
            current_unaggregated
        );

        // If we've succeeded, exit early
        if current_ravs_created > initial_ravs_created {
            println!(
                "✅ TEST PASSED: RAVs created increased from {} to {}!",
                initial_ravs_created, current_ravs_created
            );
            return Ok(());
        }

        if current_unaggregated < initial_unaggregated * 0.9 {
            println!(
                "✅ TEST PASSED: Unaggregated fees decreased significantly from {} to {}!",
                initial_unaggregated, current_unaggregated
            );
            return Ok(());
        }
    }

    println!("\n=== Summary ===");
    println!("Total receipts sent successfully: {}", total_successful);
    println!(
        "Total value sent: {} GRT",
        (MAX_RECEIPT_VALUE as f64 * total_successful as f64) / GRT_BASE as f64
    );

    // If we got here, test failed
    println!("❌ TEST FAILED: No RAV generation detected");
    Err(anyhow::anyhow!("Failed to detect RAV generation"))
}
