// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use thegraph_core::{
    alloy::{
        hex::ToHexExt,
        primitives::{Address, U256},
        signers::local::PrivateKeySigner,
    },
    CollectionId,
};

use crate::{
    constants::{
        ACCOUNT0_SECRET, CHAIN_ID, GRAPH_URL, INDEXER_URL, KAFKA_SERVERS, MAX_RECEIPT_VALUE,
        SUBGRAPH_ID, TAP_VERIFIER_CONTRACT,
    },
    database_checker::{DatabaseChecker, TapVersion},
    test_config::TestConfig,
    utils::{
        create_client_query_report, create_request, create_tap_receipt,
        encode_v2_receipt_for_header, find_allocation, GatewayReceiptSigner, KafkaReporter,
    },
};

const WAIT_TIME_BATCHES: u64 = 40;
const NUM_RECEIPTS: u32 = 30;

// Send receipts in batches with a delay in between
// to ensure some receipts get outside the timestamp buffer
const BATCHES: u32 = 15;
const MAX_TRIGGERS: usize = 20000;

// Function to test the tap RAV generation
// Function to test the TAP RAV generation for V1 using the database checker
pub async fn test_tap_rav_v1() -> Result<()> {
    use crate::constants::TEST_SUBGRAPH_DEPLOYMENT;

    // Setup HTTP client and env-backed config
    let http_client = Arc::new(Client::new());
    let cfg = TestConfig::from_env()?;

    // Payer address (lowercase hex without 0x)
    let payer = Address::from_str(&cfg.account0_address)?;
    let payer_hex = format!("{:x}", payer);

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), &cfg.graph_url).await?;
    let allocation_id = Address::from_str(&allocation_id)?;
    let allocation_id_hex = format!("{:x}", allocation_id);

    println!("Found allocation ID: {allocation_id}");

    // Setup database checker
    let mut db_checker = DatabaseChecker::new(cfg.clone()).await?;

    // Initial DB snapshot for V1 with current configuration
    println!("\n=== V1 Initial State (Config) ===");
    db_checker.print_summary(&payer_hex, TapVersion::V1).await?;

    let initial_state = db_checker.get_state(&payer_hex, TapVersion::V1).await?;
    let initial_pending_value = db_checker
        .get_pending_receipt_value(&allocation_id_hex, &payer_hex, TapVersion::V1)
        .await?;

    // Get trigger threshold from tap-agent configuration
    let trigger_threshold = db_checker.get_trigger_value_wei()?;
    println!(
        "\n🔧 Using configured trigger threshold: {} wei ({:.6} GRT)",
        trigger_threshold,
        trigger_threshold as f64 / 1e18
    );

    println!("\n=== V1 STAGE 1: Sending large receipt batches with small pauses ===");

    // Send multiple receipts in batches with a gap between them
    let mut total_successful = 0;
    // Track previous counts for clearer progress output
    let mut prev_rav_count = initial_state.rav_count;
    let mut prev_receipt_count = initial_state.receipt_count;

    for batch in 0..BATCHES {
        let batch_num = batch + 1;
        println!(
            "Sending batch {} of {} with {} receipts each...",
            batch_num, BATCHES, NUM_RECEIPTS
        );
        println!(
            "post to {}/api/deployments/id/{}",
            cfg.gateway_url, TEST_SUBGRAPH_DEPLOYMENT
        );

        for i in 0..NUM_RECEIPTS {
            let response = http_client
                .post(format!(
                    "{}/api/deployments/id/{}",
                    cfg.gateway_url, TEST_SUBGRAPH_DEPLOYMENT
                ))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            if response.status().is_success() {
                total_successful += 1;
                println!("Query {} of batch {} sent successfully", i + 1, batch_num);
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to send query: {}",
                    response.status()
                ));
            }

            // Small pause between queries within batch
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Check V1 database state after batch
        let batch_state = db_checker.get_state(&payer_hex, TapVersion::V1).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&allocation_id_hex, &payer_hex, TapVersion::V1)
            .await?;

        // Calculate progress toward trigger threshold using config
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "After V1 batch {}: RAVs: {} → {}, Receipts: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            batch_num,
            prev_rav_count,
            batch_state.rav_count,
            prev_receipt_count,
            batch_state.receipt_count,
            current_pending_value,
            progress_pct
        );

        // Update previous counters for next iteration
        prev_rav_count = batch_state.rav_count;
        prev_receipt_count = batch_state.receipt_count;

        // Check if RAV was already created after this batch
        if batch_state.rav_count > initial_state.rav_count {
            println!(
                "✅ V1 RAV CREATED after batch {}! RAVs: {} → {}",
                batch_num, initial_state.rav_count, batch_state.rav_count
            );
            return Ok(());
        }

        // Wait between batches - long enough for first batch to exit buffer
        if batch < BATCHES - 1 {
            println!("Waiting for buffer period + 5s...");
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES)).await;
        }
    }

    println!("\n=== V1 STAGE 2: Sending continuous trigger receipts ===");

    // Now send a series of regular queries with short intervals until RAV is detected
    for i in 0..MAX_TRIGGERS {
        println!("Sending trigger query {}/{}...", i + 1, MAX_TRIGGERS);

        let response = http_client
            .post(format!(
                "{}/api/deployments/id/{}",
                cfg.gateway_url, TEST_SUBGRAPH_DEPLOYMENT
            ))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
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

        // Check after each trigger using DB
        tokio::time::sleep(Duration::from_secs(1)).await;

        let current_state = db_checker.get_state(&payer_hex, TapVersion::V1).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&allocation_id_hex, &payer_hex, TapVersion::V1)
            .await?;

        // Calculate progress toward trigger threshold using config
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "After V1 trigger {}: RAVs: {} → {}, Receipts: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            i + 1,
            prev_rav_count,
            current_state.rav_count,
            prev_receipt_count,
            current_state.receipt_count,
            current_pending_value,
            progress_pct
        );

        // Update previous counters for next iteration
        prev_rav_count = current_state.rav_count;
        prev_receipt_count = current_state.receipt_count;

        // Success conditions
        if current_state.rav_count > initial_state.rav_count {
            println!(
                "✅ V1 TEST PASSED: RAVs created increased from {} to {}!",
                initial_state.rav_count, current_state.rav_count
            );
            return Ok(());
        }

        // Check if pending value decreased significantly (RAV was created and cleared pending receipts)
        let initial_pending_f64: f64 = initial_pending_value.to_string().parse().unwrap_or(0.0);
        if current_pending_f64 < initial_pending_f64 - (threshold_f64 / 2.0) {
            println!(
                "✅ V1 TEST PASSED: Unaggregated fees decreased significantly from {} to {} wei!",
                initial_pending_value, current_pending_value
            );
            return Ok(());
        }
    }

    println!("\n=== V1 Summary ===");
    println!("Total V1 queries sent successfully: {total_successful}");

    // Final state check using database
    let final_state = db_checker.get_state(&payer_hex, TapVersion::V1).await?;
    let final_pending_value = db_checker
        .get_pending_receipt_value(&allocation_id_hex, &payer_hex, TapVersion::V1)
        .await?;

    println!(
        "Final V1 state: RAVs: {} (started: {}), Receipts: {} (started: {}), Pending value: {} wei (started: {} wei)",
        final_state.rav_count,
        initial_state.rav_count,
        final_state.receipt_count,
        initial_state.receipt_count,
        final_pending_value,
        initial_pending_value
    );

    // Print detailed breakdown for debugging
    db_checker
        .print_detailed_summary(&payer_hex, TapVersion::V1)
        .await?;

    // Alternative success condition: Wait a bit longer for delayed RAV creation
    println!("\n=== Waiting for delayed V1 RAV creation (30s timeout) ===");
    let rav_created = db_checker
        .wait_for_rav_creation(
            &payer_hex,
            initial_state.rav_count,
            30, // 30 second timeout
            2,  // check every 2 seconds
            TapVersion::V1,
        )
        .await?;

    if rav_created {
        let final_state_after_wait = db_checker.get_state(&payer_hex, TapVersion::V1).await?;
        println!(
            "✅ V1 TEST PASSED: RAV CREATED (delayed)! RAVs: {} → {}",
            initial_state.rav_count, final_state_after_wait.rav_count
        );
        return Ok(());
    }

    // Timestamp buffer diagnosis using tap-agent config
    db_checker
        .diagnose_timestamp_buffer(&payer_hex, &allocation_id_hex, TapVersion::V1)
        .await?;

    // If we got here, test failed
    println!("❌ V1 TEST FAILED: No RAV generation detected");
    Err(anyhow::anyhow!("Failed to detect V1 RAV generation"))
}

pub async fn test_invalid_chain_id() -> Result<()> {
    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse().unwrap();

    let http_client = Arc::new(Client::new());

    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;

    let allocation_id = Address::from_str(&allocation_id)?;
    println!("Found allocation ID: {allocation_id}");

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
        format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}").as_str(),
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

// Function to test the TAP RAV generation with V2 receipts using database checker
pub async fn test_tap_rav_v2() -> Result<()> {
    // Setup HTTP client and env-backed test config
    let http_client = Arc::new(Client::new());
    let cfg = TestConfig::from_env()?;
    let payer = Address::from_str(&cfg.account0_address)?;

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), &cfg.graph_url).await?;
    let allocation_id = Address::from_str(&allocation_id)?;

    // Setup database checker
    let mut db_checker = DatabaseChecker::new(cfg.clone()).await?;

    // Format addresses for database queries
    let payer_hex = format!("{:x}", payer);
    let collection_id = CollectionId::from(allocation_id);
    let collection_id_hex = collection_id.encode_hex();

    // Check initial state with current configuration
    println!("\n=== V2 Initial State (Config) ===");
    db_checker.print_summary(&payer_hex, TapVersion::V2).await?;

    let initial_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let initial_pending_value = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;
    let initial_rav_value = initial_state.rav_value.clone();

    // Get trigger threshold from tap-agent configuration
    let trigger_threshold = db_checker.get_trigger_value_wei()?;
    let receipts_needed = trigger_threshold / (MAX_RECEIPT_VALUE / 10); // Using trigger receipt value
    println!(
        "\n🔧 Using configured trigger threshold: {} wei ({:.6} GRT)",
        trigger_threshold,
        trigger_threshold as f64 / 1e18
    );
    let receipt_value = MAX_RECEIPT_VALUE / 10;
    println!(
        "📊 Receipts needed for trigger: ~{receipts_needed} receipts at {receipt_value} wei each",
    );

    println!("\n=== V2 STAGE 1: Sending large receipt batches with small pauses ===");

    println!("{cfg:?}");

    // Send multiple V2 receipts in batches with a gap between them
    let mut total_successful = 0;
    // Track previous counts for clearer progress output
    let mut prev_rav_count = initial_state.rav_count;
    let mut prev_receipt_count = initial_state.receipt_count;

    for batch in 0..BATCHES {
        let batch_num = batch + 1;
        println!("Sending V2 batch {batch_num} of {BATCHES} with {NUM_RECEIPTS} receipts each...");

        for _ in 0..NUM_RECEIPTS {
            // Create and send a V2 receipt via the gateway
            let response = http_client
                .post(format!(
                    "{}/api/deployments/id/{}",
                    cfg.gateway_url, cfg.test_subgraph_deployment
                ))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            if response.status().is_success() {
                total_successful += 1;
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to send V2 query: {}",
                    response.status()
                ));
            }

            // Small pause between queries within batch
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Check database state after batch
        let batch_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
            .await?;

        // Calculate progress toward trigger threshold
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "After V2 batch {}: RAVs: {} → {}, Receipts: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            batch_num,
            prev_rav_count,
            batch_state.rav_count,
            prev_receipt_count,
            batch_state.receipt_count,
            current_pending_value,
            progress_pct
        );

        // Update previous counters for next iteration
        prev_rav_count = batch_state.rav_count;
        prev_receipt_count = batch_state.receipt_count;

        // Enhanced check if RAV was created or updated after this batch
        let (rav_created, rav_increased) = db_checker
            .check_v2_rav_progress(
                &payer_hex,
                initial_state.rav_count,
                &initial_rav_value,
                TapVersion::V2,
            )
            .await?;

        if rav_created {
            println!(
                "✅ V2 RAV CREATED after batch {}! RAVs: {} → {}",
                batch_num, initial_state.rav_count, batch_state.rav_count
            );
            return Ok(());
        }

        if rav_increased {
            println!(
                "✅ V2 RAV UPDATED after batch {}! Value: {} → {} wei",
                batch_num, initial_rav_value, batch_state.rav_value
            );
            return Ok(());
        }

        // Wait between batches - long enough for first batch to exit buffer
        if batch < BATCHES - 1 {
            println!("Waiting for buffer period + 5s...");
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES)).await;
        }
    }

    println!("\n=== V2 STAGE 2: Sending continuous trigger receipts ===");

    // Now send a series of regular V2 queries with short intervals until RAV is detected
    for i in 0..MAX_TRIGGERS {
        println!("Sending V2 trigger query {}/{}...", i + 1, MAX_TRIGGERS);

        let response = http_client
            .post(format!(
                "{}/api/deployments/id/{}",
                cfg.gateway_url, cfg.test_subgraph_deployment
            ))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
            .json(&json!({ "query": "{ _meta { block { number } } }" }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if response.status().is_success() {
            total_successful += 1;
            println!("V2 Trigger receipt {} sent successfully", i + 1);
        } else {
            return Err(anyhow::anyhow!(
                "Failed to send V2 trigger query: {}",
                response.status()
            ));
        }

        // Check after each trigger
        tokio::time::sleep(Duration::from_secs(1)).await;

        let current_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
            .await?;

        // Calculate progress toward trigger threshold
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "After V2 trigger {}: RAVs: {} → {}, Receipts: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            i + 1,
            prev_rav_count,
            current_state.rav_count,
            prev_receipt_count,
            current_state.receipt_count,
            current_pending_value,
            progress_pct
        );

        // Update previous counters for next iteration
        prev_rav_count = current_state.rav_count;
        prev_receipt_count = current_state.receipt_count;

        // Enhanced success conditions for V2
        let (rav_created, rav_increased) = db_checker
            .check_v2_rav_progress(
                &payer_hex,
                initial_state.rav_count,
                &initial_rav_value,
                TapVersion::V2,
            )
            .await?;

        if rav_created {
            println!(
                "✅ V2 TEST PASSED: New RAV created! RAVs: {} → {}",
                initial_state.rav_count, current_state.rav_count
            );
            return Ok(());
        }

        if rav_increased {
            println!(
                "✅ V2 TEST PASSED: Existing RAV value increased! {} → {} wei",
                initial_rav_value, current_state.rav_value
            );
            return Ok(());
        }

        // Check if pending value decreased significantly (RAV was created and cleared pending receipts)
        let initial_pending_f64: f64 = initial_pending_value.to_string().parse().unwrap_or(0.0);
        if current_pending_f64 < initial_pending_f64 - (threshold_f64 / 2.0) {
            println!(
                "✅ V2 TEST PASSED: Unaggregated fees decreased significantly from {} to {} wei!",
                initial_pending_value, current_pending_value
            );
            return Ok(());
        }
    }

    println!("\n=== V2 Summary ===");
    println!("Total V2 queries sent successfully: {total_successful}");

    // Final state check using database
    let final_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let final_pending_value = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!(
        "Final V2 state: RAVs: {} (started: {}), Receipts: {} (started: {}), Pending value: {} wei (started: {} wei)",
        final_state.rav_count,
        initial_state.rav_count,
        final_state.receipt_count,
        initial_state.receipt_count,
        final_pending_value,
        initial_pending_value
    );

    // Print detailed breakdown for debugging
    db_checker
        .print_detailed_summary(&payer_hex, TapVersion::V2)
        .await?;

    // Enhanced alternative success condition: Use wait_for_rav_creation_or_update with timeout
    println!("\n=== Waiting for delayed RAV creation or update (30s timeout) ===");
    let (rav_created, rav_updated) = db_checker
        .wait_for_rav_creation_or_update(
            &payer_hex,
            initial_state.rav_count,
            initial_rav_value.clone(),
            30, // 30 second timeout
            2,  // check every 2 seconds
            TapVersion::V2,
        )
        .await?;

    if rav_created || rav_updated {
        let final_state_after_wait = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        if rav_created {
            println!(
                "✅ V2 TEST PASSED: RAV CREATED (delayed)! RAVs: {} → {}",
                initial_state.rav_count, final_state_after_wait.rav_count
            );
        } else {
            println!(
                "✅ V2 TEST PASSED: RAV UPDATED (delayed)! Value: {} → {} wei",
                initial_rav_value, final_state_after_wait.rav_value
            );
        }
        return Ok(());
    }

    // If we got here, test failed
    println!("❌ V2 TEST FAILED: No RAV generation detected");
    println!(
        "V2 Receipts accumulated: {} → {} (+{})",
        initial_state.receipt_count,
        final_state.receipt_count,
        final_state.receipt_count - initial_state.receipt_count
    );

    // Timestamp buffer diagnosis using tap-agent config
    db_checker
        .diagnose_timestamp_buffer(&payer_hex, &collection_id_hex, TapVersion::V2)
        .await?;

    Err(anyhow::anyhow!("Failed to detect V2 RAV generation"))
}

pub async fn test_direct_service_rav_v2() -> Result<()> {
    // Same constants as before
    const MAX_RECEIPT_VALUE_V2: u128 = 200_000_000_000_000; // 0.0002 GRT per receipt
    const NUM_RECEIPTS_V2: u32 = 12; // 12 receipts per batch = 0.0024 GRT total
    const BATCHES_V2: u32 = 2; // Just 2 batches total
    const WAIT_TIME_BATCHES_V2: u64 = 60; // Wait 35 seconds for buffer exit
    const MAX_TRIGGERS_V2: usize = 3; // Only need a few triggers after threshold is met

    // Setup HTTP client and env-backed config
    let http_client = Arc::new(Client::new());
    let cfg = TestConfig::from_env()?;

    // Create gateway-compatible signer from private key (Account 0)
    let gateway_signer: PrivateKeySigner = cfg.account0_secret.parse()?;
    println!("Gateway signer address: {:?}", gateway_signer.address());

    // Verify this matches ACCOUNT0_ADDRESS from env
    let expected_address = Address::from_str(&cfg.account0_address)?;
    if gateway_signer.address() != expected_address {
        return Err(anyhow::anyhow!(
            "Gateway signer address mismatch! Expected: {:?}, Got: {:?}",
            expected_address,
            gateway_signer.address()
        ));
    }
    println!("✅ Gateway signer matches ACCOUNT0_ADDRESS from env");

    // Create receipt signer with V2 configuration
    let receipt_signer = GatewayReceiptSigner::new(
        gateway_signer,
        U256::from(cfg.chain_id),
        Address::from_str(&cfg.graph_tally_collector_contract)?,
    );

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), &cfg.graph_url).await?;
    println!("Found allocation ID: {allocation_id}");
    let allocation_id = Address::from_str(&allocation_id)?;

    // Convert allocation to collection for V2
    let collection_id = CollectionId::from(allocation_id);
    let payer = receipt_signer.payer_address();
    let service_provider = allocation_id;
    let data_service = Address::from_str(&cfg.test_data_service)?;

    // Create unified database checker
    let mut db_checker = DatabaseChecker::new(cfg.clone()).await?;
    println!("✅ DatabaseChecker connected to: {}", cfg.database_url());

    // Get trigger threshold from tap-agent configuration
    let rav_trigger_threshold = db_checker.get_trigger_value_wei()?;

    println!("Direct Service Test Configuration:");
    println!("  Allocation ID: {allocation_id:?}");
    println!("  Collection ID: {collection_id:?}");
    println!("  Payer (Gateway): {payer:?}");
    println!("  Service Provider: {service_provider:?}");
    println!("  Data Service: {data_service:?}");
    println!(
        "  Using GraphTallyCollector: {}",
        cfg.graph_tally_collector_contract
    );
    println!("  Receipt value: {} wei (0.0002 GRT)", MAX_RECEIPT_VALUE_V2);
    println!(
        "  Expected total per batch: {} wei (0.0024 GRT)",
        NUM_RECEIPTS_V2 as u128 * MAX_RECEIPT_VALUE_V2
    );
    println!(
        "  🔧 RAV trigger threshold: {} wei ({:.6} GRT)",
        rav_trigger_threshold,
        rav_trigger_threshold as f64 / 1e18
    );

    // Create Kafka reporter for publishing receipt data
    let mut kafka_reporter = KafkaReporter::new(KAFKA_SERVERS)?;
    println!("✅ Kafka reporter connected to: {KAFKA_SERVERS}");

    // Format addresses as hex strings for database queries
    let payer_hex = format!("{:x}", payer);
    let collection_id_hex = collection_id.encode_hex();
    let service_provider_hex = format!("{:x}", service_provider);
    let data_service_hex = format!("{:x}", data_service);

    // Check initial state with current configuration
    println!("\n=== Initial Database State (Config) ===");
    db_checker.print_summary(&payer_hex, TapVersion::V2).await?;

    // Focus on V2 for this test
    let initial_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let initial_v2_rav_value = initial_v2_state.rav_value.clone();

    // Check if this specific collection already has a RAV
    let has_existing_rav = db_checker
        .has_rav_for_identifier(
            &collection_id_hex,
            &payer_hex,
            &service_provider_hex,
            &data_service_hex,
            TapVersion::V2,
        )
        .await?;

    if has_existing_rav {
        println!("⚠️  Collection already has a RAV, this may affect test results");
    }

    let initial_pending_value = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;
    println!(
        "📊 Initial pending receipt value for this collection: {} wei",
        initial_pending_value
    );

    println!("\n=== DIRECT SERVICE STAGE 1: Sending optimized receipt batches ===");

    // Send multiple V2 receipts directly to the service
    let mut total_successful = 0;

    for batch in 0..BATCHES_V2 {
        let batch_num = batch + 1;
        println!(
            "Sending Direct Service batch {} of {} with {} receipts each (0.0002 GRT per receipt)...",
            batch_num, BATCHES_V2, NUM_RECEIPTS_V2
        );

        for i in 0..NUM_RECEIPTS_V2 {
            // Create V2 receipt using gateway's signer
            let receipt = receipt_signer.create_receipt(
                collection_id,
                MAX_RECEIPT_VALUE_V2,
                payer,
                data_service,
                service_provider,
            )?;

            let receipt_encoded = encode_v2_receipt_for_header(&receipt)?;

            // Send via gateway deployments endpoint so receipts are processed end-to-end
            let response = http_client
                .post(format!(
                    "{}/api/deployments/id/{}",
                    cfg.gateway_url, cfg.test_subgraph_deployment
                ))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
                .header("tap-receipt", receipt_encoded)
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            if response.status().is_success() {
                total_successful += 1;
                if (i + 1) % 4 == 0 || i == NUM_RECEIPTS_V2 - 1 {
                    println!(
                        "  ✓ Direct Service Query {} of batch {} sent successfully",
                        i + 1,
                        batch_num
                    );
                }

                // Publish receipt data to Kafka
                let query_id = format!("direct-service-batch-{}-query-{}", batch_num, i + 1);
                let report = create_client_query_report(
                    query_id,
                    payer,
                    allocation_id,
                    Some(collection_id),
                    MAX_RECEIPT_VALUE_V2,
                    100, // response_time_ms
                    &format!(
                        "{}/api/deployments/id/{}",
                        cfg.gateway_url, cfg.test_subgraph_deployment
                    ),
                    &cfg.gateway_api_key,
                );

                if let Err(e) = kafka_reporter.publish_to_topic("gateway_queries", &report) {
                    println!("⚠️  Failed to publish to Kafka: {}", e);
                }
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to send direct service query: {}",
                    response.status()
                ));
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Check V2 database state after batch
        let batch_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
            .await?;

        // Calculate progress toward trigger threshold using config
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = rav_trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "📊 After Direct Service batch {}: V2 RAVs: {} → {}, Pending value for collection: {} wei ({:.1}% of trigger threshold)",
            batch_num,
            initial_v2_state.rav_count,
            batch_v2_state.rav_count,
            current_pending_value,
            progress_pct
        );

        // Enhanced check if RAV was created or updated after this batch
        let (rav_created, rav_increased) = db_checker
            .check_v2_rav_progress(
                &payer_hex,
                initial_v2_state.rav_count,
                &initial_v2_rav_value,
                TapVersion::V2,
            )
            .await?;

        if rav_created {
            println!(
                "✅ V2 RAV CREATED after batch {}! RAVs: {} → {}",
                batch_num, initial_v2_state.rav_count, batch_v2_state.rav_count
            );
            db_checker
                .print_detailed_summary(&payer_hex, TapVersion::V2)
                .await?;
            return Ok(());
        }

        if rav_increased {
            println!(
                "✅ V2 RAV UPDATED after batch {}! Value: {} → {} wei",
                batch_num, initial_v2_rav_value, batch_v2_state.rav_value
            );
            db_checker
                .print_detailed_summary(&payer_hex, TapVersion::V2)
                .await?;
            return Ok(());
        }

        // Wait between batches
        if batch < BATCHES_V2 - 1 {
            println!(
                "⏱️  Waiting {} seconds for timestamp buffer to clear...",
                WAIT_TIME_BATCHES_V2
            );
            tokio::time::sleep(Duration::from_secs(WAIT_TIME_BATCHES_V2)).await;
        }
    }

    println!("\n=== DIRECT SERVICE STAGE 2: Final trigger receipts ===");

    // Add a small delay to ensure all receipts from Stage 1 are out of the buffer
    println!("⏱️  Waiting 5 seconds before sending trigger receipts...");
    tokio::time::sleep(Duration::from_secs(60)).await;

    let pre_trigger_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let pre_trigger_pending = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!(
        "📊 Before triggers: V2 RAVs: {}, Pending value for collection: {} wei",
        pre_trigger_v2_state.rav_count, pre_trigger_pending
    );
    let mut prev_rav_count = pre_trigger_v2_state.rav_count;

    // Send trigger receipts
    for i in 0..MAX_TRIGGERS_V2 {
        println!(
            "Sending Direct Service trigger query {}/{}...",
            i + 1,
            MAX_TRIGGERS_V2
        );

        let trigger_receipt_value = MAX_RECEIPT_VALUE_V2 / 4; // 0.00005 GRT per trigger
        let receipt = receipt_signer.create_receipt(
            collection_id,
            trigger_receipt_value,
            payer,
            data_service,
            service_provider,
        )?;

        let receipt_encoded = encode_v2_receipt_for_header(&receipt)?;

        let response = http_client
            .post(format!(
                "{}/api/deployments/id/{}",
                cfg.gateway_url, cfg.test_subgraph_deployment
            ))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", cfg.gateway_api_key))
            .header("tap-receipt", receipt_encoded)
            .json(&json!({
                "query": "{ _meta { block { number } } }"
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if response.status().is_success() {
            total_successful += 1;
            println!(
                "  ✓ Direct Service trigger receipt {} sent successfully",
                i + 1
            );

            // Publish trigger receipt to Kafka
            let query_id = format!("direct-service-trigger-{}", i + 1);
            let report = create_client_query_report(
                query_id,
                payer,
                allocation_id,
                Some(collection_id),
                trigger_receipt_value,
                100, // response_time_ms
                &format!(
                    "{}/api/deployments/id/{}",
                    cfg.gateway_url, cfg.test_subgraph_deployment
                ),
                &cfg.gateway_api_key,
            );

            if let Err(e) = kafka_reporter.publish_to_topic("gateway_queries", &report) {
                println!("⚠️  Failed to publish trigger receipt to Kafka: {}", e);
            }
        } else {
            return Err(anyhow::anyhow!(
                "Failed to send direct service trigger query: {}",
                response.status()
            ));
        }

        // Check after each trigger with a longer delay
        tokio::time::sleep(Duration::from_secs(2)).await;

        let current_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        let current_pending_value = db_checker
            .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
            .await?;

        // Calculate progress toward trigger threshold using config
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = rav_trigger_threshold as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "📊 After Direct Service trigger {}: V2 RAVs: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            i + 1,
            prev_rav_count,
            current_v2_state.rav_count,
            current_pending_value,
            progress_pct
        );

        // Update previous counter for next iteration
        prev_rav_count = current_v2_state.rav_count;

        // Enhanced success conditions for V2
        let (rav_created, rav_increased) = db_checker
            .check_v2_rav_progress(
                &payer_hex,
                initial_v2_state.rav_count,
                &initial_v2_rav_value,
                TapVersion::V2,
            )
            .await?;

        if rav_created {
            println!(
                "✅ V2 RAV CREATED after trigger {}! RAVs: {} → {}",
                i + 1,
                initial_v2_state.rav_count,
                current_v2_state.rav_count
            );
            db_checker
                .print_detailed_summary(&payer_hex, TapVersion::V2)
                .await?;
            return Ok(());
        }

        if rav_increased {
            println!(
                "✅ V2 RAV UPDATED after trigger {}! Value: {} → {} wei",
                i + 1,
                initial_v2_rav_value,
                current_v2_state.rav_value
            );
            db_checker
                .print_detailed_summary(&payer_hex, TapVersion::V2)
                .await?;
            return Ok(());
        }

        // Check if pending value decreased significantly (RAV was created and cleared pending receipts)
        let initial_pending_f64: f64 = initial_pending_value.to_string().parse().unwrap_or(0.0);
        if current_pending_f64 < initial_pending_f64 - (threshold_f64 / 2.0) {
            println!(
                "✅ V2 PENDING VALUE DECREASED significantly! {} → {} wei (RAV likely created)",
                initial_pending_value, current_pending_value
            );

            // Print final detailed state
            db_checker
                .print_detailed_summary(&payer_hex, TapVersion::V2)
                .await?;
            return Ok(());
        }
    }

    println!("\n=== Direct Service Summary ===");
    println!(
        "📊 Total Direct Service queries sent successfully: {}",
        total_successful
    );
    println!(
        "📊 Expected total value sent: {} wei",
        (BATCHES_V2 as u128 * NUM_RECEIPTS_V2 as u128 * MAX_RECEIPT_VALUE_V2)
            + (MAX_TRIGGERS_V2 as u128 * MAX_RECEIPT_VALUE_V2 / 4)
    );
    println!(
        "📊 RAV trigger threshold: {} wei ({:.6} GRT)",
        rav_trigger_threshold,
        rav_trigger_threshold as f64 / 1e18
    );

    // Final state check using database
    let final_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let final_pending_value = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!(
        "📊 Final V2 state: RAVs: {} (started: {}), Pending value: {} wei (started: {} wei)",
        final_v2_state.rav_count,
        initial_v2_state.rav_count,
        final_pending_value,
        initial_pending_value
    );

    // Print final detailed breakdown
    db_checker
        .print_detailed_summary(&payer_hex, TapVersion::V2)
        .await?;

    // Enhanced alternative success condition: Use wait_for_rav_creation_or_update with timeout
    println!("\n=== Waiting for delayed RAV creation or update (30s timeout) ===");
    let (rav_created, rav_updated) = db_checker
        .wait_for_rav_creation_or_update(
            &payer_hex,
            initial_v2_state.rav_count,
            initial_v2_rav_value.clone(),
            30, // 30 second timeout
            2,  // check every 2 seconds
            TapVersion::V2,
        )
        .await?;

    if rav_created || rav_updated {
        let final_state_after_wait = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        if rav_created {
            println!(
                "✅ V2 RAV CREATED (delayed)! RAVs: {} → {}",
                initial_v2_state.rav_count, final_state_after_wait.rav_count
            );
        } else {
            println!(
                "✅ V2 RAV UPDATED (delayed)! Value: {} → {} wei",
                initial_v2_rav_value, final_state_after_wait.rav_value
            );
        }

        // Print final detailed state
        db_checker
            .print_detailed_summary(&payer_hex, TapVersion::V2)
            .await?;
        return Ok(());
    }

    // If we got here, test failed
    println!("❌ V2 TEST FAILED: No RAV creation detected");
    println!("💡 This confirms V2 RAV triggering logic may not be working properly");
    println!(
        "📊 V2 Receipts accumulated: {} → {} (+{})",
        initial_v2_state.receipt_count,
        final_v2_state.receipt_count,
        final_v2_state.receipt_count - initial_v2_state.receipt_count
    );

    // Print combined summary for debugging
    println!("\n=== Final Combined State for Debugging ===");
    db_checker.print_combined_summary(&payer_hex).await?;

    println!("\n💡 Debug suggestions:");
    println!("   - Check tap-agent logs for 'Error while getting the heaviest allocation'");
    println!("   - Verify timestamp buffer is working (receipts older than 30s)");
    println!("   - Check if allocation is blocked due to ongoing RAV request");
    println!("   - Verify V2 RAV triggering threshold configuration");
    println!("   - Check if V2 horizon tables are being populated correctly");

    Err(anyhow::anyhow!(
        "Failed to detect V2 RAV generation after sending {} receipts with {} wei total value",
        total_successful,
        final_pending_value
    ))
}
