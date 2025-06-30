// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

use crate::{
    constants::{
        ACCOUNT0_SECRET, CHAIN_ID, GATEWAY_API_KEY, GATEWAY_URL, GRAPH_URL, INDEXER_URL,
        MAX_RECEIPT_VALUE, SUBGRAPH_ID, TAP_AGENT_METRICS_URL, TAP_VERIFIER_CONTRACT,
    },
    utils::{create_request, create_tap_receipt, find_allocation},
    MetricsChecker,
};

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
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;

    let allocation_id = Address::from_str(&allocation_id)?;

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

pub async fn test_invalid_chain_id() -> Result<()> {
    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse().unwrap();

    let http_client = Arc::new(Client::new());

    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;

    let allocation_id = Address::from_str(&allocation_id)?;
    println!("Found allocation ID: {}", allocation_id);

    let receipt = create_tap_receipt(
        MAX_RECEIPT_VALUE,
        &allocation_id,
        TAP_VERIFIER_CONTRACT,
        CHAIN_ID + 18,
        &wallet,
    )?;

    let receipt_json = serde_json::to_string(&receipt).unwrap();
    let response = create_request(
        &http_client,
        format!("{}/subgraphs/id/{}", INDEXER_URL, SUBGRAPH_ID).as_str(),
        &receipt_json,
        &json!({
            "query": "{ _meta { block { number } } }"
        }),
    )
    .send()
    .await?;

    assert!(
        response.status().is_client_error(),
        "Failed to send receipt"
    );

    Ok(())
}
