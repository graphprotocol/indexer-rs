// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use thegraph_core::alloy::{
    hex::ToHexExt,
    primitives::{Address, U256},
    signers::local::PrivateKeySigner,
};
use thegraph_core::CollectionId;

use crate::{
    constants::{
        ACCOUNT0_SECRET, CHAIN_ID, GATEWAY_API_KEY, GATEWAY_PRIVATE_KEY, GATEWAY_URL,
        GRAPH_TALLY_COLLECTOR_CONTRACT, GRAPH_URL, INDEXER_URL, KAFKA_SERVERS, MAX_RECEIPT_VALUE,
        SUBGRAPH_ID, TAP_AGENT_METRICS_URL, TAP_VERIFIER_CONTRACT, TEST_DATA_SERVICE,
    },
    utils::{
        create_client_query_report, create_request, create_tap_receipt, create_tap_receipt_v2,
        encode_v2_receipt, encode_v2_receipt_for_header, find_allocation, GatewayReceiptSigner,
        KafkaReporter,
    },
    MetricsChecker,
};

use crate::database_checker::{DatabaseChecker, TapVersion};

const WAIT_TIME_BATCHES: u64 = 40;
const NUM_RECEIPTS: u32 = 30; // Increased to 30 receipts per batch

// Send receipts in batches with a delay in between
// to ensure some receipts get outside the timestamp buffer
const BATCHES: u32 = 15; // Increased to 15 batches for total 450 receipts in Stage 1
const MAX_TRIGGERS: usize = 200; // Increased trigger attempts to 200

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
        "\n=== Initial metrics: RAVs created: {initial_ravs_created}, Unaggregated fees: {initial_unaggregated} ==="
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
                .post(format!("{GATEWAY_URL}/api/subgraphs/id/{SUBGRAPH_ID}"))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {GATEWAY_API_KEY}"))
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
            .post(format!("{GATEWAY_URL}/api/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {GATEWAY_API_KEY}"))
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
                "✅ TEST PASSED: RAVs created increased from {initial_ravs_created} to {current_ravs_created}!"
            );
            return Ok(());
        }

        if current_unaggregated < initial_unaggregated * 0.9 {
            println!(
                "✅ TEST PASSED: Unaggregated fees decreased significantly from {initial_unaggregated} to {current_unaggregated}!"
            );
            return Ok(());
        }
    }

    println!("\n=== Summary ===");
    println!("Total queries sent successfully: {total_successful}");

    // If we got here, test failed
    println!("❌ TEST FAILED: No RAV generation detected");
    Err(anyhow::anyhow!("Failed to detect RAV generation"))
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

// Function to test the TAP RAV generation with V2 receipts
pub async fn test_tap_rav_v2() -> Result<()> {
    // Setup HTTP client
    let http_client = Arc::new(Client::new());
    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse().unwrap();

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;
    let allocation_id = Address::from_str(&allocation_id)?;

    // For V2, we need payer and service provider addresses
    let payer = wallet.address();
    let service_provider = allocation_id; // Using allocation_id as service provider for simplicity

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
        "\n=== V2 Initial metrics: RAVs created: {initial_ravs_created}, Unaggregated fees: {initial_unaggregated} ==="
    );

    // Calculate expected thresholds
    let trigger_threshold = 2_000_000_000_000_000u128; // 0.002 GRT trigger value
    let receipts_needed = trigger_threshold / (MAX_RECEIPT_VALUE / 10); // Using trigger receipt value
    println!("📊 RAV trigger threshold: {trigger_threshold} wei (0.002 GRT)",);
    let receipt_value = MAX_RECEIPT_VALUE / 10;
    println!(
        "📊 Receipts needed for trigger: ~{receipts_needed} receipts at {receipt_value} wei each",
    );

    println!("\n=== V2 STAGE 1: Sending large receipt batches with small pauses ===");

    // Send multiple V2 receipts in two batches with a gap between them
    let mut total_successful = 0;

    for batch in 0..BATCHES {
        let batch = batch + 1;
        println!("Sending V2 batch {batch} of {BATCHES} with {NUM_RECEIPTS} receipts each...",);

        for i in 0..NUM_RECEIPTS {
            // Create V2 receipt
            let receipt = create_tap_receipt_v2(
                MAX_RECEIPT_VALUE,
                &allocation_id,
                GRAPH_TALLY_COLLECTOR_CONTRACT,
                CHAIN_ID,
                &wallet,
                &payer,
                &service_provider,
            )?;

            let receipt_encoded = encode_v2_receipt(&receipt)?;

            let response = http_client
                .post(format!("{GATEWAY_URL}/api/subgraphs/id/{SUBGRAPH_ID}"))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {GATEWAY_API_KEY}"))
                .header("Tap-Receipt", receipt_encoded)
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await?;

            if response.status().is_success() {
                total_successful += 1;
                println!(
                    "V2 Query {} of batch {} sent successfully",
                    i + 1,
                    batch + 1
                );
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to send V2 query: {}",
                    response.status()
                ));
            }

            // Small pause between queries within batch
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Check metrics after batch
        let batch_metrics = metrics_checker.get_current_metrics().await?;
        let current_unaggregated =
            batch_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());
        let trigger_threshold = 2_000_000_000_000_000u128;
        let progress_pct =
            (current_unaggregated as f64 / trigger_threshold as f64 * 100.0).min(100.0);

        println!(
            "After V2 batch {}: RAVs created: {}, Unaggregated fees: {} ({:.1}% of trigger threshold)",
            batch + 1,
            batch_metrics.ravs_created_by_allocation(&allocation_id.to_string()),
            current_unaggregated,
            progress_pct
        );

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

        // Create V2 receipt
        let receipt = create_tap_receipt_v2(
            MAX_RECEIPT_VALUE / 10, // Smaller value for trigger receipts
            &allocation_id,
            GRAPH_TALLY_COLLECTOR_CONTRACT,
            CHAIN_ID,
            &wallet,
            &payer,
            &service_provider,
        )?;

        let receipt_encoded = encode_v2_receipt(&receipt)?;

        let response = http_client
            .post(format!("{GATEWAY_URL}/api/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {GATEWAY_API_KEY}"))
            .header("Tap-Receipt", receipt_encoded)
            .json(&json!({
                "query": "{ _meta { block { number } } }"
            }))
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

        let current_metrics = metrics_checker.get_current_metrics().await?;
        let current_ravs_created =
            current_metrics.ravs_created_by_allocation(&allocation_id.to_string());
        let current_unaggregated =
            current_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

        // Calculate progress toward trigger threshold
        let trigger_threshold = 2_000_000_000_000_000u128;
        let progress_pct =
            (current_unaggregated as f64 / trigger_threshold as f64 * 100.0).min(100.0);

        println!(
            "After V2 trigger {}: RAVs created: {}, Unaggregated fees: {} ({:.1}% of trigger threshold)",
            i + 1,
            current_ravs_created,
            current_unaggregated,
            progress_pct
        );

        // If we've succeeded, exit early
        if current_ravs_created > initial_ravs_created {
            println!(
                "✅ V2 TEST PASSED: RAVs created increased from {initial_ravs_created} to {current_ravs_created}!"
            );
            return Ok(());
        }

        if current_unaggregated < initial_unaggregated * 0.9 {
            println!(
                "✅ V2 TEST PASSED: Unaggregated fees decreased significantly from {initial_unaggregated} to {current_unaggregated}!"
            );
            return Ok(());
        }
    }

    println!("\n=== V2 Summary ===");
    println!("Total V2 queries sent successfully: {total_successful}");

    // If we got here, test failed
    println!("❌ V2 TEST FAILED: No RAV generation detected");
    Err(anyhow::anyhow!("Failed to detect V2 RAV generation"))
}

pub async fn test_direct_service_rav_v2() -> Result<()> {
    // Same constants as before
    const MAX_RECEIPT_VALUE_V2: u128 = 200_000_000_000_000; // 0.0002 GRT per receipt
    const NUM_RECEIPTS_V2: u32 = 12; // 12 receipts per batch = 0.0024 GRT total
    const BATCHES_V2: u32 = 2; // Just 2 batches total
    const WAIT_TIME_BATCHES_V2: u64 = 60; // Wait 35 seconds for buffer exit
    const MAX_TRIGGERS_V2: usize = 3; // Only need a few triggers after threshold is met
    const RAV_TRIGGER_THRESHOLD: u128 = 2_000_000_000_000_000; // 0.002 GRT in wei

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Create gateway-compatible signer from private key
    let gateway_signer: PrivateKeySigner = GATEWAY_PRIVATE_KEY.parse()?;
    println!("Gateway signer address: {:?}", gateway_signer.address());

    // Verify this matches ACCOUNT0_ADDRESS from env
    let expected_address = Address::from_str("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266")?;
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
        U256::from(CHAIN_ID),
        Address::from_str(GRAPH_TALLY_COLLECTOR_CONTRACT)?,
    );

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;
    println!("Found allocation ID: {allocation_id}");
    let allocation_id = Address::from_str(&allocation_id)?;

    // Convert allocation to collection for V2
    let collection_id = CollectionId::from(allocation_id);
    let payer = receipt_signer.payer_address();
    let service_provider = allocation_id;
    let data_service = Address::from_str(TEST_DATA_SERVICE)?;

    println!("Direct Service Test Configuration:");
    println!("  Allocation ID: {allocation_id:?}");
    println!("  Collection ID: {collection_id:?}");
    println!("  Payer (Gateway): {payer:?}");
    println!("  Service Provider: {service_provider:?}");
    println!("  Data Service: {data_service:?}");
    println!("  Using GraphTallyCollector: {GRAPH_TALLY_COLLECTOR_CONTRACT}");
    println!("  Receipt value: {} wei (0.0002 GRT)", MAX_RECEIPT_VALUE_V2);
    println!(
        "  Expected total per batch: {} wei (0.0024 GRT)",
        NUM_RECEIPTS_V2 as u128 * MAX_RECEIPT_VALUE_V2
    );
    println!(
        "  RAV trigger threshold: {} wei (0.002 GRT)",
        RAV_TRIGGER_THRESHOLD
    );

    // Create unified database checker
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| crate::constants::POSTGRES_URL.to_string());

    let db_checker = DatabaseChecker::new(&database_url).await?;
    println!("✅ DatabaseChecker connected to: {}", database_url);

    // Create Kafka reporter for publishing receipt data
    let mut kafka_reporter = KafkaReporter::new(KAFKA_SERVERS)?;
    println!("✅ Kafka reporter connected to: {KAFKA_SERVERS}");

    // Format addresses as hex strings for database queries
    let payer_hex = format!("{:x}", payer);
    let collection_id_hex = collection_id.encode_hex();
    let service_provider_hex = format!("{:x}", service_provider);
    let data_service_hex = format!("{:x}", data_service);

    // Check initial state for both V1 and V2
    println!("\n=== Initial Database State ===");
    db_checker.print_combined_summary(&payer_hex).await?;

    // Focus on V2 for this test
    let initial_v2_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    println!("\n=== V2 Detailed Initial State ===");
    db_checker
        .print_detailed_summary(&payer_hex, TapVersion::V2)
        .await?;

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

            // Send directly to the service endpoint
            let response = http_client
                .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
                .header("Content-Type", "application/json")
                .header("Tap-Receipt", receipt_encoded)
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
                    &format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"),
                    GATEWAY_API_KEY,
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

        // Calculate progress toward trigger threshold
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = RAV_TRIGGER_THRESHOLD as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "📊 After Direct Service batch {}: V2 RAVs: {} → {}, Pending value for collection: {} wei ({:.1}% of trigger threshold)",
            batch_num,
            initial_v2_state.rav_count,
            batch_v2_state.rav_count,
            current_pending_value,
            progress_pct
        );

        // Check if RAV was already created after this batch
        if batch_v2_state.rav_count > initial_v2_state.rav_count {
            println!(
                "✅ V2 RAV CREATED after batch {}! RAVs: {} → {}",
                batch_num, initial_v2_state.rav_count, batch_v2_state.rav_count
            );

            // Print final detailed state
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
            .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
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
                &format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"),
                GATEWAY_API_KEY,
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

        // Calculate progress toward trigger threshold
        let current_pending_f64: f64 = current_pending_value.to_string().parse().unwrap_or(0.0);
        let threshold_f64 = RAV_TRIGGER_THRESHOLD as f64;
        let progress_pct = (current_pending_f64 / threshold_f64 * 100.0).min(100.0);

        println!(
            "📊 After Direct Service trigger {}: V2 RAVs: {} → {}, Pending value: {} wei ({:.1}% of trigger threshold)",
            i + 1,
            initial_v2_state.rav_count,
            current_v2_state.rav_count,
            current_pending_value,
            progress_pct
        );

        // Success conditions
        if current_v2_state.rav_count > initial_v2_state.rav_count {
            println!(
                "✅ V2 RAV CREATED after trigger {}! RAVs: {} → {}",
                i + 1,
                initial_v2_state.rav_count,
                current_v2_state.rav_count
            );

            // Print final detailed state
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
        "📊 RAV trigger threshold: {} wei (0.002 GRT)",
        RAV_TRIGGER_THRESHOLD
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

    // Alternative success condition: Use wait_for_rav_creation with timeout
    println!("\n=== Waiting for delayed RAV creation (30s timeout) ===");
    let rav_created = db_checker
        .wait_for_rav_creation(
            &payer_hex,
            initial_v2_state.rav_count,
            30, // 30 second timeout
            2,  // check every 2 seconds
            TapVersion::V2,
        )
        .await?;

    if rav_created {
        let final_state_after_wait = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
        println!(
            "✅ V2 RAV CREATED (delayed)! RAVs: {} → {}",
            initial_v2_state.rav_count, final_state_after_wait.rav_count
        );

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

/// Function to test TAP RAV generation by sending receipts directly to the service
/// This bypasses the gateway and creates receipts signed with the gateway's key
pub async fn test_direct_service_rav_v2_simplified() -> Result<()> {
    // Simplified constants - just 5 receipts with higher values
    const NUM_RECEIPTS: u32 = 5;
    const RECEIPT_VALUE: u128 = 500_000_000_000_000; // 0.0005 GRT per receipt
    const TOTAL_EXPECTED: u128 = 2_500_000_000_000_000; // 0.0025 GRT total
    const RAV_TRIGGER_THRESHOLD: u128 = 2_000_000_000_000_000; // 0.002 GRT
    const BUFFER_WAIT_TIME: u64 = 35; // Wait for timestamp buffer to clear

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Create gateway signer
    let gateway_signer: PrivateKeySigner = GATEWAY_PRIVATE_KEY.parse()?;
    println!("Gateway signer address: {:?}", gateway_signer.address());

    // Create receipt signer
    let receipt_signer = GatewayReceiptSigner::new(
        gateway_signer,
        U256::from(CHAIN_ID),
        Address::from_str(GRAPH_TALLY_COLLECTOR_CONTRACT)?,
    );

    // Find allocation
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;
    println!("Found allocation ID: {allocation_id}");
    let allocation_id = Address::from_str(&allocation_id)?;

    // Setup V2 parameters
    let collection_id = CollectionId::from(allocation_id);
    let payer = receipt_signer.payer_address();
    let service_provider = allocation_id;
    let data_service = Address::from_str(TEST_DATA_SERVICE)?;

    println!("\n📋 TEST CONFIGURATION:");
    println!("══════════════════════════════════════════");
    println!("  Receipts to send: {}", NUM_RECEIPTS);
    println!("  Value per receipt: {} wei (0.0005 GRT)", RECEIPT_VALUE);
    println!("  Total value: {} wei (0.0025 GRT)", TOTAL_EXPECTED);
    println!(
        "  RAV trigger threshold: {} wei (0.002 GRT)",
        RAV_TRIGGER_THRESHOLD
    );
    println!("  Collection ID: {collection_id:?}");
    println!("  Payer: {payer:?}");
    println!("══════════════════════════════════════════\n");

    // Database setup
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| crate::constants::POSTGRES_URL.to_string());
    let db_checker = DatabaseChecker::new(&database_url).await?;

    // Format addresses for database
    let payer_hex = format!("{:x}", payer);
    let collection_id_hex = collection_id.encode_hex();

    // Check initial state
    println!("📊 INITIAL STATE:");
    let initial_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let initial_pending = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!("  V2 RAVs: {}", initial_state.rav_count);
    println!("  V2 Receipts: {}", initial_state.receipt_count);
    println!("  Pending value: {} wei", initial_pending);
    println!();

    // Send the 5 receipts
    println!("🚀 SENDING {} RECEIPTS:", NUM_RECEIPTS);
    println!("══════════════════════════════════════════");

    for i in 0..NUM_RECEIPTS {
        let receipt_num = i + 1;

        // Create V2 receipt
        let receipt = receipt_signer.create_receipt(
            collection_id,
            RECEIPT_VALUE,
            payer,
            data_service,
            service_provider,
        )?;

        let receipt_encoded = encode_v2_receipt_for_header(&receipt)?;

        // Send to service
        let response = http_client
            .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
            .header("Tap-Receipt", receipt_encoded)
            .json(&json!({
                "query": "{ _meta { block { number } } }"
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if response.status().is_success() {
            let total_sent = receipt_num as u128 * RECEIPT_VALUE;
            let percentage = (total_sent as f64 / RAV_TRIGGER_THRESHOLD as f64 * 100.0).min(100.0);

            println!(
                "  Receipt {}/{}: ✅ Sent {} wei | Total: {} wei ({:.1}% of threshold)",
                receipt_num, NUM_RECEIPTS, RECEIPT_VALUE, total_sent, percentage
            );

            // Check if we've crossed the threshold
            if total_sent >= RAV_TRIGGER_THRESHOLD && receipt_num < NUM_RECEIPTS {
                println!("  🎯 Threshold crossed! Should trigger RAV soon...");
            }
        } else {
            return Err(anyhow::anyhow!(
                "Failed to send receipt {}: {}",
                receipt_num,
                response.status()
            ));
        }

        // Small delay between receipts
        tokio::time::sleep(Duration::from_millis(75)).await;
    }

    println!("══════════════════════════════════════════\n");

    // Check state after sending all receipts
    println!("📊 STATE AFTER SENDING RECEIPTS:");
    let after_send_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let after_send_pending = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!(
        "  V2 RAVs: {} (was: {})",
        after_send_state.rav_count, initial_state.rav_count
    );
    println!(
        "  V2 Receipts: {} (was: {})",
        after_send_state.receipt_count, initial_state.receipt_count
    );
    println!(
        "  Pending value: {} wei (was: {} wei)",
        after_send_pending, initial_pending
    );

    // Check if RAV was already created
    if after_send_state.rav_count > initial_state.rav_count {
        println!("\n✅ SUCCESS: RAV created immediately!");
        return Ok(());
    }

    // ******************************************

    // Send one trigger receipt after buffer wait
    println!("\n🎯 SENDING TRIGGER RECEIPT:");
    let trigger_value = 100_000_000_000_000; // 0.0001 GRT
    let trigger_receipt = receipt_signer.create_receipt(
        collection_id,
        trigger_value,
        payer,
        data_service,
        service_provider,
    )?;

    let trigger_encoded = encode_v2_receipt_for_header(&trigger_receipt)?;

    let response = http_client
        .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
        .header("Content-Type", "application/json")
        .header("tap-receipt", trigger_encoded)
        .json(&json!({
            "query": "{ _meta { block { number } } }"
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    if response.status().is_success() {
        println!("  Trigger receipt sent successfully");
    }
    //************************************

    // Wait for timestamp buffer to clear
    println!(
        "\n⏱️  WAITING {} SECONDS FOR TIMESTAMP BUFFER...",
        BUFFER_WAIT_TIME
    );
    for i in 0..BUFFER_WAIT_TIME {
        if i % 10 == 0 {
            println!("  {} seconds elapsed...", i);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Check periodically
        if i % 5 == 0 {
            let current_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
            if current_state.rav_count > initial_state.rav_count {
                println!("\n✅ SUCCESS: RAV created after {} seconds!", i);
                return Ok(());
            }
        }
    }

    // Send one trigger receipt after buffer wait
    println!("\n🎯 SENDING TRIGGER RECEIPT:");
    let trigger_value = 100_000_000_000_000; // 0.0001 GRT
    let trigger_receipt = receipt_signer.create_receipt(
        collection_id,
        trigger_value,
        payer,
        data_service,
        service_provider,
    )?;

    let trigger_encoded = encode_v2_receipt_for_header(&trigger_receipt)?;

    let response = http_client
        .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
        .header("Content-Type", "application/json")
        .header("tap-receipt", trigger_encoded)
        .json(&json!({
            "query": "{ _meta { block { number } } }"
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    if response.status().is_success() {
        println!("  Trigger receipt sent successfully");
    }

    // Final check with timeout
    println!("\n⏳ WAITING FOR RAV CREATION (30s timeout):");
    let rav_created = db_checker
        .wait_for_rav_creation(&payer_hex, initial_state.rav_count, 30, 2, TapVersion::V2)
        .await?;

    // Final state
    println!("\n📊 FINAL STATE:");
    let final_state = db_checker.get_state(&payer_hex, TapVersion::V2).await?;
    let final_pending = db_checker
        .get_pending_receipt_value(&collection_id_hex, &payer_hex, TapVersion::V2)
        .await?;

    println!(
        "  V2 RAVs: {} (started: {})",
        final_state.rav_count, initial_state.rav_count
    );
    println!(
        "  V2 Receipts: {} (started: {})",
        final_state.receipt_count, initial_state.receipt_count
    );
    println!(
        "  Pending value: {} wei (started: {} wei)",
        final_pending, initial_pending
    );

    if rav_created || final_state.rav_count > initial_state.rav_count {
        println!("\n✅ SUCCESS: V2 RAV was created!");
        println!("  Receipts sent: {}", NUM_RECEIPTS + 1);
        println!("  Total value sent: {} wei", TOTAL_EXPECTED + trigger_value);
        return Ok(());
    }

    // Test failed
    println!("\n❌ FAILED: No RAV creation detected");
    println!("\n🔍 DEBUGGING HINTS:");
    println!("  1. Check tap-agent logs for 'Error while getting the heaviest allocation'");
    println!("  2. Look for 'No valid allocation found' warnings");
    println!("  3. Check for 'too much fees under their buffer' errors");
    println!("  4. Verify the allocation exists in fee tracker");
    println!("  5. Check if receipts are being converted correctly between Horizon/Legacy");

    // Print detailed breakdown for debugging
    println!("\n📋 DETAILED V2 STATE:");
    db_checker
        .print_detailed_summary(&payer_hex, TapVersion::V2)
        .await?;

    db_checker
        .diagnose_timestamp_buffer(
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266", // payer
            &collection_id_hex,
            30, // buffer_seconds (from your config)
            TapVersion::V2,
        )
        .await?;

    Err(anyhow::anyhow!(
        "V2 RAV generation failed after sending {} receipts worth {} wei",
        NUM_RECEIPTS + 1,
        TOTAL_EXPECTED + trigger_value
    ))
}
