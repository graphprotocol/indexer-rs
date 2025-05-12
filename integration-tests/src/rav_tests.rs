// Copyright 2023-, Edge i Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thegraph_core::alloy::primitives::Address;

use crate::MetricsChecker;

// TODO: Would be nice to read this values from:
// contrib/tap-agent/config.toml
// and contrib/local-network/.env
const GATEWAY_URL: &str = "http://localhost:7700";
const SUBGRAPH_ID: &str = "BFr2mx7FgkJ36Y6pE5BiXs1KmNUmVDCnL82KUSdcLW1g";
const GATEWAY_API_KEY: &str = "deadbeefdeadbeefdeadbeefdeadbeef";
const TAP_AGENT_METRICS_URL: &str = "http://localhost:7300/metrics";

const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

const WAIT_TIME_BATCHES: u64 = 40;

const NUM_RECEIPTS: u32 = 3;

// Send receipts in batches with a delay in between
// to ensure some receipts get outside the timestamp buffer
const BATCHES: u32 = 2;
const MAX_TRIGGERS: usize = 100;

// Function to test the tap RAV generation
pub async fn test_tap_rav_v1() -> Result<()> {
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

    println!("\n=== STAGE 1: Sending large receipt batches with small pauses ===");

    // Send multiple receipts in two batches with a gap between them
    let mut total_successful = 0;

    for batch in 0..BATCHES {
        println!(
            "Sending batch {} of 2 with {} receipts each...",
            batch + 1,
            NUM_RECEIPTS
        );

        for i in 0..NUM_RECEIPTS {
            let response = http_client
                .post(format!("{}/api/subgraphs/id/{}", GATEWAY_URL, SUBGRAPH_ID))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", GATEWAY_API_KEY))
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            if response.status().is_success() {
                total_successful += 1;
                println!("Query {} of batch {} sent successfully", i + 1, batch + 1);
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to send query: {}",
                    response.status()
                ));
            }

            // Small pause between queries within batch
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
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES)).await;
        }
    }

    println!("\n=== STAGE 2: Sending continuous trigger receipts ===");

    // Now send a series of regular queries with short intervals until RAV is detected
    for i in 0..MAX_TRIGGERS {
        println!("Sending trigger query {}/{}...", i + 1, MAX_TRIGGERS);

        let response = http_client
            .post(format!("{}/api/subgraphs/id/{}", GATEWAY_URL, SUBGRAPH_ID))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", GATEWAY_API_KEY))
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
                "Failed to send trigger query: {}",
                response.status()
            ));
        }

        // Check after each trigger
        tokio::time::sleep(Duration::from_secs(1)).await;

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
    println!("Total queries sent successfully: {}", total_successful);

    // If we got here, test failed
    println!("❌ TEST FAILED: No RAV generation detected");
    Err(anyhow::anyhow!("Failed to detect RAV generation"))
}
