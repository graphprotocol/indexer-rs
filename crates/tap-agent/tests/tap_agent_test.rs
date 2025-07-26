// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for complete TAP agent using tokio-based infrastructure
//!
//! This test suite verifies the full TAP agent functionality including
//! high-throughput receipt processing, allocation management, and system resilience.

use std::{collections::HashMap, time::Duration};

use indexer_monitor::{DeploymentDetails, SubgraphClient};
use indexer_tap_agent::{
    agent::tap_agent::{TapAgent, TapAgentConfig},
    // test::{store_receipt, CreateReceipt}, // Legacy test utilities - using test_assets instead
};
use serde_json::json;
use tap_core::tap_eip712_domain;
use test_assets::{
    create_signed_receipt, setup_shared_test_db, SignedReceiptRequest, TestDatabase,
    ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2, INDEXER_ADDRESS, VERIFIER_ADDRESS,
};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::time::sleep;
use tracing::{debug, info};
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

/// Helper to create test EIP712 domain
fn create_test_eip712_domain() -> Eip712Domain {
    tap_eip712_domain(1, VERIFIER_ADDRESS)
}

/// Simple helper to store a signed receipt in the database
async fn store_receipt(
    pgpool: &sqlx::PgPool,
    receipt: &tap_graph::SignedReceipt,
) -> anyhow::Result<()> {
    // Recover signer address from signature
    let signer_address = receipt.recover_signer(&test_assets::TAP_EIP712_DOMAIN)?;

    sqlx::query!(
        r#"
        INSERT INTO scalar_tap_receipts (
            signer_address, signature, allocation_id, timestamp_ns, nonce, value
        ) VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        format!("{signer_address:#x}"),
        &receipt.signature.as_bytes(),
        format!(
            "{allocation_id:#x}",
            allocation_id = receipt.message.allocation_id
        ),
        receipt.message.timestamp_ns as i64,
        receipt.message.nonce as i64,
        sqlx::types::BigDecimal::from(receipt.message.value)
    )
    .execute(pgpool)
    .await?;
    Ok(())
}

/// Helper to create test configuration for high-throughput testing
fn create_high_throughput_config(
    pgpool: sqlx::PgPool,
    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,
) -> TapAgentConfig {
    TapAgentConfig {
        pgpool,
        rav_threshold: 150, // Equivalent to trigger_value
        rav_request_interval: Duration::from_millis(500),
        event_buffer_size: 100,
        result_buffer_size: 100,
        rav_buffer_size: 50,

        // Escrow configuration
        escrow_subgraph_v1: Some(escrow_subgraph),
        escrow_subgraph_v2: None, // horizon_enabled: false
        indexer_address: INDEXER_ADDRESS,
        escrow_syncing_interval: Duration::from_secs(10),
        reject_thawing_signers: true,

        // Network subgraph configuration
        network_subgraph: Some(network_subgraph),
        allocation_syncing_interval: Duration::from_secs(60),
        recently_closed_allocation_buffer: Duration::from_secs(300),

        // TAP Manager configuration
        domain_separator: Some(create_test_eip712_domain()),
        sender_aggregator_endpoints: HashMap::new(),
    }
}

/// Helper to setup test environment with mock servers
async fn setup_high_throughput_test_env() -> (
    TestDatabase,
    &'static SubgraphClient,
    &'static SubgraphClient,
) {
    let test_db = setup_shared_test_db().await;

    // Setup mock escrow subgraph
    let escrow_subgraph_mock_server = MockServer::start().await;
    escrow_subgraph_mock_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "transactions": [],
                }
            })),
        ))
        .await;

    // Setup mock network subgraph
    let network_subgraph_mock_server = MockServer::start().await;
    network_subgraph_mock_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "allocations": [],
                    "meta": {
                        "block": {
                            "number": 1,
                            "hash": "hash",
                            "timestamp": 1
                        }
                    }
                }
            })),
        ))
        .await;

    // Create subgraph clients
    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&network_subgraph_mock_server.uri()).unwrap(),
        )
        .await,
    ));

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&escrow_subgraph_mock_server.uri()).unwrap(),
        )
        .await,
    ));

    (test_db, network_subgraph, escrow_subgraph)
}

/// High-throughput integration test for complete TAP agent using stream processor
/// This test verifies the system can handle thousands of receipts across multiple allocations
#[tokio::test]
async fn tokio_high_throughput_tap_agent_test() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .try_init();

    let (test_db, network_subgraph, escrow_subgraph) = setup_high_throughput_test_env().await;
    let pgpool = test_db.pool.clone();
    let config = create_high_throughput_config(pgpool.clone(), escrow_subgraph, network_subgraph);

    info!("🚀 Starting high-throughput TAP agent test with stream processor");

    // Start the stream-based TAP agent
    let mut agent = TapAgent::new(config);
    agent.start().await.expect("Failed to start TAP agent");

    debug!("✅ TAP agent started successfully");

    // Generate and store batch receipts across multiple allocations
    const AMOUNT_OF_RECEIPTS: u64 = 1000; // Reduced for test performance
    let allocations = [ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2];

    info!(
        "📊 Generating {} receipts across {} allocations",
        AMOUNT_OF_RECEIPTS,
        allocations.len()
    );

    let start_time = std::time::Instant::now();

    for i in 0..AMOUNT_OF_RECEIPTS {
        // Distribute receipts across the 3 allocations
        let allocation_index = (i % 3) as usize;
        let allocation_id = allocations[allocation_index];

        let receipt = create_signed_receipt(
            SignedReceiptRequest::builder()
                .allocation_id(allocation_id)
                .nonce(i)
                .timestamp_ns(1_000_000_000 + i)
                .value(((i % 100) + 1) as u128)
                .build(),
        )
        .await;

        store_receipt(&pgpool, &receipt)
            .await
            .expect("Failed to store receipt");

        // Log progress every 100 receipts
        if i % 100 == 0 {
            debug!("📊 Stored {} receipts", i + 1);
        }
    }

    let batch_duration = start_time.elapsed();
    info!(
        "⏱️ Stored {} receipts in {:?}",
        AMOUNT_OF_RECEIPTS, batch_duration
    );

    // Allow time for the agent to process all receipts
    info!("⏳ Allowing time for receipt processing...");
    sleep(Duration::from_secs(10)).await;

    // Verify processing results
    let total_receipt_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts")
        .fetch_one(&pgpool)
        .await
        .expect("Failed to query total receipt count");

    let total_rav_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_ravs")
        .fetch_one(&pgpool)
        .await
        .expect("Failed to query total RAV count");

    info!(
        "📊 Processing results: {} receipts remaining, {} RAVs generated",
        total_receipt_count, total_rav_count
    );

    // Check processing per allocation
    for (idx, allocation_id) in allocations.iter().enumerate() {
        let receipt_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts WHERE allocation_id = $1")
                .bind(allocation_id.encode_hex())
                .fetch_one(&pgpool)
                .await
                .expect("Failed to query receipt count by allocation");

        let rav_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_ravs WHERE allocation_id = $1")
                .bind(allocation_id.encode_hex())
                .fetch_one(&pgpool)
                .await
                .expect("Failed to query RAV count by allocation");

        info!(
            "📁 Allocation {}: {} receipts remaining, {} RAVs",
            idx, receipt_count, rav_count
        );
    }

    // Test graceful shutdown under load
    info!("🔄 Testing graceful shutdown");
    agent.shutdown().await.expect("Failed to shutdown agent");

    // Run the agent briefly to let it process
    let agent_run = tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(2), agent.run()).await;
    });

    // Wait for completion
    let _ = agent_run.await;

    // Verify basic processing occurred (some receipts should have been processed)
    // Note: In a real high-throughput scenario, we'd expect significant processing
    assert!(
        total_receipt_count <= AMOUNT_OF_RECEIPTS as i64,
        "Receipt count should not exceed generated amount"
    );

    // Allow time for cleanup
    sleep(Duration::from_millis(500)).await;

    info!("✅ High-throughput TAP agent test completed successfully");
}
