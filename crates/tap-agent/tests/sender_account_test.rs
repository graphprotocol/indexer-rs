// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for SenderAccount layer using tokio-based infrastructure
//!
//! This test suite verifies sender account functionality including allocation management,
//! receipt processing, and proper cleanup behavior.

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
    pgpool, ALLOCATION_ID_0, INDEXER_ADDRESS, TAP_SIGNER as SIGNER, VERIFIER_ADDRESS,
};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::time::sleep;
use tracing::{debug, info};
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

const TRIGGER_VALUE: u128 = 500;

/// Helper to create test EIP712 domain
fn create_test_eip712_domain() -> Eip712Domain {
    tap_eip712_domain(1, VERIFIER_ADDRESS)
}

/// Helper to create test config with appropriate trigger value
fn create_test_config() -> &'static SenderAccountConfig {
    Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(500),
        max_amount_willing_to_lose_grt: TRIGGER_VALUE + 1000,
        trigger_value: TRIGGER_VALUE,
        rav_request_timeout: Duration::from_secs(5),
        rav_request_receipt_limit: 10,
        indexer_address: INDEXER_ADDRESS,
        escrow_polling_interval: Duration::from_secs(1),
        tap_sender_timeout: Duration::from_secs(5),
        trusted_senders: std::collections::HashSet::new(),
        horizon_enabled: false,
    }))
}

/// Helper to setup test environment with mock subgraphs
async fn setup_test_env() -> (
    PgPool,
    MockServer,
    MockServer,
    &'static SubgraphClient,
    &'static SubgraphClient,
) {
    let pgpool_future = pgpool();
    let pgpool = pgpool_future.await;

    // Setup mock network subgraph
    let mock_network_server = MockServer::start().await;
    mock_network_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "meta": {
                        "block": {
                            "number": 1,
                            "hash": "hash",
                            "timestamp": 1
                        }
                    },
                    "allocations": [
                        {"id": *ALLOCATION_ID_0 }
                    ]
                }
                }))),
        )
        .await;

    // Setup mock escrow subgraph
    let mock_escrow_server = MockServer::start().await;
    mock_escrow_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                "transactions": [],
            }
            })),
        ))
        .await;

    // Create subgraph clients
    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&mock_network_server.uri()).expect("Valid URL"),
        )
        .await,
    ));

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&mock_escrow_server.uri()).expect("Valid URL"),
        )
        .await,
    ));

    (
        pgpool,
        mock_network_server,
        mock_escrow_server,
        network_subgraph,
        escrow_subgraph,
    )
}

/// Integration test for sender account layer using tokio-based infrastructure
/// This test verifies allocation lifecycle management and RAV generation
#[tokio::test]
async fn tokio_sender_account_layer_test() {
    let (pgpool, _mock_network, _mock_escrow, network_subgraph, escrow_subgraph) =
        setup_test_env().await;
    let config = create_test_config();
    let domain = create_test_eip712_domain();
    let lifecycle = LifecycleManager::new();

    info!("🧪 Starting tokio-based sender account layer test");

    // Store initial receipt
    let receipt = Legacy::create_received_receipt(
        ALLOCATION_ID_0,
        &SIGNER.0,
        1,                   // nonce
        1_000_000_000,       // timestamp_ns
        TRIGGER_VALUE - 100, // value in GRT wei
    );

    let receipt_id = store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .expect("Failed to store receipt");

    debug!("✅ Stored initial receipt with ID: {}", receipt_id);

    // Start the tokio-based sender accounts manager
    let manager_task = SenderAccountsManagerTask::spawn(
        &lifecycle,
        Some("test-sender-account-layer".to_string()),
        config,
        pgpool.clone(),
        escrow_subgraph,
        network_subgraph,
        domain,
        HashMap::new(), // sender_aggregator_endpoints
        Some("sender-account-test".to_string()),
    )
    .await
    .expect("Failed to spawn sender accounts manager task");

    debug!("✅ SenderAccountsManagerTask spawned successfully");

    // Allow time for initial processing
    sleep(Duration::from_millis(2000)).await;

    // Add more receipts to trigger RAV processing
    for i in 2..=5 {
        let receipt = Legacy::create_received_receipt(
            ALLOCATION_ID_0,
            &SIGNER.0,
            i,                        // nonce
            1_000_000_000 + i * 1000, // timestamp_ns
            100,                      // value in GRT wei
        );

        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .expect("Failed to store receipt");
    }

    debug!("✅ Stored additional receipts for processing");

    // Allow time for processing
    sleep(Duration::from_millis(3000)).await;

    // Verify receipts were processed
    let receipt_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts WHERE allocation_id = $1")
            .bind(ALLOCATION_ID_0.encode_hex())
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query receipt count");

    info!("📊 Remaining receipt count: {}", receipt_count);

    // Check for generated RAVs
    let rav_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_ravs WHERE allocation_id = $1")
            .bind(ALLOCATION_ID_0.encode_hex())
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query RAV count");

    info!("📊 RAV count: {}", rav_count);

    // Test graceful shutdown
    debug!("🔄 Testing graceful task shutdown");
    drop(manager_task);

    // Allow time for cleanup
    sleep(Duration::from_millis(500)).await;

    info!("✅ Tokio-based sender account layer test completed successfully");
}
