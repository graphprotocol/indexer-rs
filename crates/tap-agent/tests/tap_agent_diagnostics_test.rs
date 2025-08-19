// Diagnostic test to understand connection pool behavior
use indexer_tap_agent::agent::tap_agent::{TapAgent, TapAgentConfig};
use sqlx::types::BigDecimal;
use std::{collections::HashMap, time::Duration};
use test_assets::{setup_shared_test_db, INDEXER_ADDRESS};
use thegraph_core::alloy::primitives::Address;
use tracing::info;

#[tokio::test]
async fn diagnose_connection_pool_behavior() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    info!("🔍 TDD Diagnostic: Understanding connection pool behavior");

    // Step 1: Create test database and check initial pool state
    let test_db = setup_shared_test_db().await;
    let config = TapAgentConfig {
        pgpool: test_db.pool.clone(),
        rav_threshold: 1000,
        rav_request_interval: Duration::from_millis(100),
        event_buffer_size: 10,
        result_buffer_size: 10,
        rav_buffer_size: 10,
        escrow_subgraph_v1: None,
        escrow_subgraph_v2: None,
        indexer_address: Address::from(*INDEXER_ADDRESS),
        escrow_syncing_interval: Duration::from_secs(30),
        reject_thawing_signers: true,
        network_subgraph: None,
        allocation_syncing_interval: Duration::from_secs(60),
        recently_closed_allocation_buffer: Duration::from_secs(300),
        domain_separator: None,
        sender_aggregator_endpoints: HashMap::new(),
    };
    let pool = config.pgpool.clone();

    info!("Pool size: {}", pool.size());
    info!("Pool max_size: {:?}", pool.options().get_max_connections());
    info!(
        "Pool min_connections: {:?}",
        pool.options().get_min_connections()
    );
    info!(
        "Pool acquire_timeout: {:?}",
        pool.options().get_acquire_timeout()
    );

    // Step 2: Test basic query without TAP agent
    info!("Testing basic query...");
    let result = sqlx::query!("SELECT 1 as test").fetch_one(&pool).await;

    match result {
        Ok(_) => info!("✅ Basic query succeeded"),
        Err(e) => info!("❌ Basic query failed: {}", e),
    }

    // Step 3: Test the actual allocation query
    info!("Testing allocation query...");
    let allocations_result = sqlx::query!(
        r#"
                SELECT DISTINCT allocation_id 
                FROM scalar_tap_receipts 
                ORDER BY allocation_id
            "#
    )
    .fetch_all(&pool)
    .await;

    match allocations_result {
        Ok(rows) => info!("✅ Allocation query succeeded, found {} rows", rows.len()),
        Err(e) => info!("❌ Allocation query failed: {}", e),
    }

    // Step 4: Test multiple concurrent queries
    info!("Testing concurrent queries...");
    let mut handles = vec![];

    for i in 0..5 {
        let pool_clone = pool.clone();
        let handle = tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = sqlx::query!("SELECT $1::int as num", i)
                .fetch_one(&pool_clone)
                .await;
            let elapsed = start.elapsed();

            match result {
                Ok(_) => info!("✅ Concurrent query {} succeeded in {:?}", i, elapsed),
                Err(e) => info!(
                    "❌ Concurrent query {} failed after {:?}: {}",
                    i, elapsed, e
                ),
            }
        });
        handles.push(handle);
    }

    // Wait for all queries
    for handle in handles {
        let _ = handle.await;
    }

    // Step 5: Now test TAP agent startup
    info!("Testing TAP agent startup...");
    let mut agent = TapAgent::new(config);

    match tokio::time::timeout(Duration::from_secs(5), agent.start()).await {
        Ok(Ok(())) => info!("✅ TAP agent started successfully"),
        Ok(Err(e)) => info!("❌ TAP agent start failed: {}", e),
        Err(_) => info!("❌ TAP agent start timed out after 5 seconds"),
    }

    // Close pool
    pool.close().await;

    info!("🔍 Diagnostic test complete");
}

#[tokio::test]
async fn test_isolated_allocation_query() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    info!("🔍 TDD: Testing allocation queries in isolation");

    let test_db = setup_shared_test_db().await;
    let pool = test_db.pool.clone();

    // Insert test data (without 0x prefix for CHAR(40) fields)
    sqlx::query!(
        r#"
                INSERT INTO scalar_tap_receipts 
                (allocation_id, signer_address, signature, timestamp_ns, nonce, value)
                VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        "abcdabcdabcdabcdabcdabcdabcdabcdabcdabcd",
        "1234567890123456789012345678901234567890",
        &b"test_signature"[..],
        BigDecimal::from(1000000),
        BigDecimal::from(1),
        BigDecimal::from(100)
    )
    .execute(&pool)
    .await
    .expect("Should insert test receipt");

    // Now test the allocation query directly
    let allocations = sqlx::query!(
        r#"
                SELECT DISTINCT allocation_id 
                FROM scalar_tap_receipts 
                ORDER BY allocation_id
            "#
    )
    .fetch_all(&pool)
    .await;

    match allocations {
        Ok(rows) => {
            info!(
                "✅ Allocation query succeeded, found {} allocations",
                rows.len()
            );
            assert_eq!(rows.len(), 1, "Should find one allocation");
        }
        Err(e) => {
            info!("❌ Allocation query failed: {}", e);
            panic!("Allocation query should not fail with test data");
        }
    }

    pool.close().await;
}
