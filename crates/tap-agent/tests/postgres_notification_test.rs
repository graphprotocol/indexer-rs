//! Test PostgreSQL LISTEN/NOTIFY functionality in testcontainer environment
//!
//! This test validates that our PostgreSQL notification system works correctly
//! in the testcontainer environment that our integration tests use.

use sqlx::postgres::PgListener;
use std::time::Duration;
use test_assets::setup_shared_test_db;
use tokio::time::timeout;
use tracing::info;

#[tokio::test]
async fn test_postgres_listen_notify_basic() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    info!("🔍 Testing basic PostgreSQL LISTEN/NOTIFY in testcontainer");

    let test_db = setup_shared_test_db().await;
    let pool = test_db.pool.clone();

    // Create a listener on one connection
    let mut listener = PgListener::connect_with(&pool)
        .await
        .expect("Should create PgListener");

    listener
        .listen("test_channel")
        .await
        .expect("Should listen to test_channel");

    info!("✅ PgListener created and listening to test_channel");

    // Send notification from another connection
    sqlx::query!("NOTIFY test_channel, 'test_message'")
        .execute(&pool)
        .await
        .expect("Should send notification");

    info!("✅ Notification sent via NOTIFY");

    // Try to receive the notification with timeout
    let result = timeout(Duration::from_secs(2), listener.recv()).await;

    match result {
        Ok(Ok(notification)) => {
            info!(
                "✅ Received notification: channel={}, payload={}",
                notification.channel(),
                notification.payload()
            );
            assert_eq!(notification.channel(), "test_channel");
            assert_eq!(notification.payload(), "test_message");
        }
        Ok(Err(e)) => {
            panic!("❌ Error receiving notification: {e}");
        }
        Err(_) => {
            panic!("❌ Timeout waiting for notification - PostgreSQL LISTEN/NOTIFY not working in testcontainer");
        }
    }

    pool.close().await;
    info!("✅ PostgreSQL LISTEN/NOTIFY test completed successfully");
}

#[tokio::test]
async fn test_postgres_trigger_notification() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    info!("🔍 Testing PostgreSQL trigger notifications for scalar_tap_receipts");

    let test_db = setup_shared_test_db().await;
    let pool = test_db.pool.clone();

    // Create a listener for the actual TAP receipt channel
    let mut listener = PgListener::connect_with(&pool)
        .await
        .expect("Should create PgListener");

    listener
        .listen("scalar_tap_receipt_notification")
        .await
        .expect("Should listen to scalar_tap_receipt_notification");

    info!("✅ PgListener listening to scalar_tap_receipt_notification");

    // Insert a receipt (this should trigger the database trigger)
    let test_allocation = "fa44c72b753a66591f241c7dc04e8178c30e13af"; // No 0x prefix
    let test_sender = "90f8bf6a479f320ead074411a4b0e7944ea8c9c1";

    sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts 
            (allocation_id, signer_address, signature, timestamp_ns, nonce, value)
            VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        test_allocation,
        test_sender,
        b"test_signature",
        sqlx::types::BigDecimal::from(1640995200000000000i64),
        sqlx::types::BigDecimal::from(1i64),
        sqlx::types::BigDecimal::from(500i64)
    )
    .execute(&pool)
    .await
    .expect("Should insert test receipt");

    info!("✅ Test receipt inserted - waiting for trigger notification");

    // Try to receive the trigger notification
    let result = timeout(Duration::from_secs(3), listener.recv()).await;

    match result {
        Ok(Ok(notification)) => {
            info!(
                "✅ Received trigger notification: channel={}, payload={}",
                notification.channel(),
                notification.payload()
            );

            // Verify it's the expected JSON format
            let payload = notification.payload();
            assert!(
                payload.contains(r#""allocation_id""#),
                "Should contain allocation_id field"
            );
            assert!(
                payload.contains(test_allocation),
                "Should contain test allocation ID"
            );
            assert!(payload.contains(test_sender), "Should contain test sender");
        }
        Ok(Err(e)) => {
            panic!("❌ Error receiving trigger notification: {e}");
        }
        Err(_) => {
            panic!("❌ Timeout waiting for trigger notification - Database trigger not working in testcontainer");
        }
    }

    pool.close().await;
    info!("✅ PostgreSQL trigger notification test completed successfully");
}
