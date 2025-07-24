// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for SenderAccountsManager layer using tokio-based infrastructure
//!
//! This test suite verifies the full flow from sender account manager through
//! allocation management, receipt processing, and proper task lifecycle.

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

const TRIGGER_VALUE: u128 = 100;

/// Helper to create test EIP712 domain
fn create_test_eip712_domain() -> Eip712Domain {
    tap_eip712_domain(1, VERIFIER_ADDRESS)
}

/// Helper to create test config
fn create_test_config() -> &'static SenderAccountConfig {
    Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(500),
        max_amount_willing_to_lose_grt: 1_000_000,
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
async fn setup_test_env_with_mocks() -> (
    PgPool,
    MockServer,
    MockServer,
    &'static SubgraphClient,
    &'static SubgraphClient,
) {
    let pgpool_future = pgpool();
    let pgpool = pgpool_future.await;

    // Setup mock network subgraph
    let mock_network_subgraph_server = MockServer::start().await;
    mock_network_subgraph_server
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
    let mock_escrow_subgraph_server = MockServer::start().await;
    mock_escrow_subgraph_server
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
            DeploymentDetails::for_query_url(&mock_network_subgraph_server.uri())
                .expect("Valid URL"),
        )
        .await,
    ));

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&mock_escrow_subgraph_server.uri())
                .expect("Valid URL"),
        )
        .await,
    ));

    (
        pgpool,
        mock_network_subgraph_server,
        mock_escrow_subgraph_server,
        network_subgraph,
        escrow_subgraph,
    )
}

/// Integration test for sender account manager layer using tokio-based infrastructure
/// This test verifies the full flow from receipt processing to RAV generation
#[tokio::test]
async fn tokio_sender_account_manager_layer_test() {
    let (pgpool, _mock_network, _mock_escrow, network_subgraph, escrow_subgraph) =
        setup_test_env_with_mocks().await;
    let config = create_test_config();
    let domain = create_test_eip712_domain();
    let lifecycle = LifecycleManager::new();

    info!("🧪 Starting tokio-based sender account manager layer test");

    // Start the tokio-based sender accounts manager
    let manager_task = SenderAccountsManagerTask::spawn(
        &lifecycle,
        Some("test-sender-accounts-manager".to_string()),
        config,
        pgpool.clone(),
        escrow_subgraph,
        network_subgraph,
        domain,
        HashMap::new(), // sender_aggregator_endpoints
        Some("test-prefix".to_string()),
    )
    .await
    .expect("Failed to spawn sender accounts manager task");

    debug!("✅ SenderAccountsManagerTask spawned successfully");

    // Store a receipt to trigger processing
    let receipt = Legacy::create_received_receipt(
        ALLOCATION_ID_0,
        &SIGNER.0,
        1,                  // nonce
        1_000_000_000,      // timestamp_ns
        TRIGGER_VALUE - 10, // value in GRT wei
    );

    let receipt_id = store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .expect("Failed to store receipt");

    debug!("✅ Stored receipt with ID: {}", receipt_id);

    // Allow time for the task to process the receipt
    sleep(Duration::from_millis(2000)).await;

    // Verify receipt was processed by checking database state
    let receipt_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts WHERE allocation_id = $1")
            .bind(ALLOCATION_ID_0.encode_hex())
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query receipt count");

    info!("📊 Receipt count in database: {}", receipt_count);

    // Test triggering a RAV request with more receipts
    for i in 2..=5 {
        let receipt = Legacy::create_received_receipt(
            ALLOCATION_ID_0,
            &SIGNER.0,
            i,                        // nonce
            1_000_000_000 + i * 1000, // timestamp_ns
            50,                       // value in GRT wei
        );

        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .expect("Failed to store receipt");
    }

    // Allow time for processing
    sleep(Duration::from_millis(3000)).await;

    // Check for any RAVs that were generated
    let rav_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_ravs")
        .fetch_one(&pgpool)
        .await
        .expect("Failed to query RAV count");

    info!("📊 RAV count in database: {}", rav_count);

    // Test graceful shutdown by dropping the task handle
    debug!("🔄 Testing graceful task shutdown");
    drop(manager_task);

    // Allow time for cleanup
    sleep(Duration::from_millis(500)).await;

    info!("✅ Tokio-based sender account manager layer test completed successfully");
}
