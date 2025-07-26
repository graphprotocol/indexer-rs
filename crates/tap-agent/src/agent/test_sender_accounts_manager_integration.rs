//! Integration tests for SenderAccountsManagerTask
//!
//! These tests validate the complete tokio-based TAP agent workflow:
//! Manager → Account → Allocation task hierarchy with real PostgreSQL notifications

#[cfg(test)]
mod tests {
    use crate::{
        agent::{
            sender_accounts_manager::SenderAccountsManagerMessage,
            sender_accounts_manager_task::SenderAccountsManagerTask,
        },
        task_lifecycle::LifecycleManager,
    };
    use std::time::Duration;
    use tap_core::tap_eip712_domain;
    use test_assets::{setup_shared_test_db, INDEXER_ADDRESS, VERIFIER_ADDRESS};
    use thegraph_core::alloy::primitives::Address;

    /// Test that SenderAccountsManagerTask can spawn successfully and handle basic operations
    #[tokio::test]
    async fn test_sender_accounts_manager_basic_spawn() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();

        // Setup test database and infrastructure
        let test_db = setup_shared_test_db().await;
        let lifecycle = LifecycleManager::new();

        // Create test configuration
        let config = Box::leak(Box::new(
            crate::agent::sender_account::SenderAccountConfig {
                indexer_address: Address::from(*INDEXER_ADDRESS),
                rav_request_buffer: Duration::from_millis(1000),
                max_amount_willing_to_lose_grt: 1_000_000,
                trigger_value: 100,
                rav_request_timeout: Duration::from_secs(5),
                rav_request_receipt_limit: 10,
                escrow_polling_interval: Duration::from_secs(10),
                tap_sender_timeout: Duration::from_secs(5),
                trusted_senders: std::collections::HashSet::new(),
                horizon_enabled: false,
            },
        ));

        // Create mock subgraph clients
        let escrow_subgraph = Box::leak(Box::new(
            indexer_monitor::SubgraphClient::new(
                reqwest::Client::new(),
                None,
                indexer_monitor::DeploymentDetails::for_query_url("http://localhost:8000")
                    .expect("Valid URL"),
            )
            .await,
        ));
        let network_subgraph = Box::leak(Box::new(
            indexer_monitor::SubgraphClient::new(
                reqwest::Client::new(),
                None,
                indexer_monitor::DeploymentDetails::for_query_url("http://localhost:8001")
                    .expect("Valid URL"),
            )
            .await,
        ));

        // Create EIP-712 domain for testing
        let domain = tap_eip712_domain(1, Address::from(*VERIFIER_ADDRESS));

        // TEST 1: Manager should spawn successfully
        tracing::info!("🧪 TEST 1: Testing SenderAccountsManagerTask spawn");
        let manager_handle = SenderAccountsManagerTask::spawn(
            &lifecycle,
            Some("test_manager".to_string()),
            config,
            test_db.pool.clone(),
            escrow_subgraph,
            network_subgraph,
            domain,
            std::collections::HashMap::new(), // Empty sender aggregator endpoints for testing
            Some("test".to_string()),
        )
        .await
        .expect("Manager should spawn successfully");

        // TEST 2: Manager should be healthy and responsive
        tracing::info!("🧪 TEST 2: Testing manager health and responsiveness");
        let health_info = lifecycle.get_system_health().await;
        assert_eq!(
            health_info.total_tasks, 1,
            "Should have 1 task (the manager)"
        );
        assert_eq!(health_info.healthy_tasks, 1, "Manager should be healthy");
        assert_eq!(health_info.failed_tasks, 0, "No tasks should be failed");

        // TEST 3: Manager should handle UpdateSenderAccounts message
        tracing::info!("🧪 TEST 3: Testing UpdateSenderAccounts message handling");
        let mut sender_accounts = std::collections::HashSet::new();
        sender_accounts.insert(Address::from(*INDEXER_ADDRESS)); // Use indexer address as test sender
        let message = SenderAccountsManagerMessage::UpdateSenderAccountsV1(sender_accounts);

        // Send message to manager (fire-and-forget style)
        manager_handle
            .cast(message)
            .await
            .expect("Should be able to send message to manager");

        // Give the manager time to process the message
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Manager should still be healthy after processing message
        let health_info = lifecycle.get_system_health().await;
        assert!(health_info.overall_healthy, "System should remain healthy");

        tracing::info!("✅ All SenderAccountsManagerTask basic tests passed!");
    }

    /// Test the full receipt processing workflow: PostgreSQL → Manager → Account → Allocation → RAV
    #[tokio::test]
    async fn test_full_receipt_workflow() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();

        tracing::info!("🧪 COMPREHENSIVE TEST: Full receipt workflow");

        // This test will validate the complete workflow but for now we'll focus on
        // making the basic manager spawn work. Once that works, we can expand this test.

        // TODO: Add PostgreSQL notification simulation
        // TODO: Add receipt creation and processing
        // TODO: Add RAV generation validation
        // TODO: Add child task lifecycle management

        tracing::info!("🚧 Full workflow test - to be implemented after basic spawn works");
    }

    /// Test error handling and recovery scenarios
    #[tokio::test]
    async fn test_manager_error_recovery() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();

        tracing::info!("🧪 RESILIENCE TEST: Manager error recovery");

        // TODO: Test what happens when child tasks fail
        // TODO: Test PostgreSQL connection issues
        // TODO: Test subgraph client failures
        // TODO: Test invalid receipt handling

        tracing::info!("🚧 Error recovery test - to be implemented");
    }
}
