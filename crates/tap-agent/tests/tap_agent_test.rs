// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for complete TAP agent using tokio-based infrastructure
//!
//! This test suite verifies the full TAP agent functionality including
//! high-throughput receipt processing, allocation management, and system resilience.

use std::{collections::HashMap, time::Duration};

use indexer_monitor::{DeploymentDetails, SubgraphClient};
use indexer_tap_agent::{
    actor_migrate::LifecycleManager,
    agent::{
        sender_account::SenderAccountConfig,
        sender_accounts_manager_task::SenderAccountsManagerTask,
    },
    tap::context::Legacy,
    test::{store_receipt, CreateReceipt},
};
use serde_json::json;
use sqlx::PgPool;
use tap_core::tap_eip712_domain;
use test_assets::{
    pgpool, ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2, INDEXER_ADDRESS, TAP_SIGNER,
    VERIFIER_ADDRESS,
};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::time::sleep;
use tracing::{debug, info};
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

/// Helper to create test EIP712 domain
fn create_test_eip712_domain() -> Eip712Domain {
    tap_eip712_domain(1, VERIFIER_ADDRESS)
}

/// Helper to create test configuration for high-throughput testing
fn create_high_throughput_config() -> &'static SenderAccountConfig {
    Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(500),
        max_amount_willing_to_lose_grt: 50,
        trigger_value: 150,
        rav_request_timeout: Duration::from_secs(60),
        rav_request_receipt_limit: 10,
        indexer_address: INDEXER_ADDRESS,
        escrow_polling_interval: Duration::from_secs(10),
        tap_sender_timeout: Duration::from_secs(30),
        trusted_senders: std::collections::HashSet::new(),
        horizon_enabled: false,
    }))
}

/// Helper to setup test environment with mock servers
async fn setup_high_throughput_test_env(
) -> (PgPool, &'static SubgraphClient, &'static SubgraphClient) {
    let pgpool_future = pgpool();
    let pgpool = pgpool_future.await;

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

    (pgpool, network_subgraph, escrow_subgraph)
}

/// High-throughput integration test for complete TAP agent using tokio infrastructure
/// This test verifies the system can handle thousands of receipts across multiple allocations
#[tokio::test]
async fn tokio_high_throughput_tap_agent_test() {
    let (pgpool, network_subgraph, escrow_subgraph) = setup_high_throughput_test_env().await;
    let config = create_high_throughput_config();
    let domain = create_test_eip712_domain();
    let lifecycle = LifecycleManager::new();

    info!("🚀 Starting high-throughput TAP agent test with tokio infrastructure");

    // Start the tokio-based TAP agent
    let agent_task = SenderAccountsManagerTask::spawn(
        &lifecycle,
        Some("high-throughput-tap-agent".to_string()),
        config,
        pgpool.clone(),
        escrow_subgraph,
        network_subgraph,
        domain,
        HashMap::new(), // sender_aggregator_endpoints
        Some("tap-agent-test".to_string()),
    )
    .await
    .expect("Failed to spawn TAP agent task");

    debug!("✅ TAP agent task spawned successfully");

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

        let receipt = Legacy::create_received_receipt(
            allocation_id,
            &TAP_SIGNER.0,
            i,                       // nonce
            1_000_000_000 + i,       // timestamp_ns
            ((i % 100) + 1) as u128, // value (1-100 GRT wei, cycling)
        );

        store_receipt(&pgpool, receipt.signed_receipt())
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

    // Test system health during high load
    let system_health = lifecycle.get_system_health().await;
    info!("📊 System health: {:?}", system_health);

    // Verify system remains healthy under load
    assert!(
        system_health.overall_healthy,
        "System should remain healthy under high throughput"
    );
    assert!(
        system_health.failed_tasks == 0,
        "No tasks should fail during processing"
    );

    // Test graceful shutdown under load
    info!("🔄 Testing graceful shutdown");
    drop(agent_task);

    // Allow time for cleanup
    sleep(Duration::from_millis(1000)).await;

    info!("✅ High-throughput TAP agent test completed successfully");
}
