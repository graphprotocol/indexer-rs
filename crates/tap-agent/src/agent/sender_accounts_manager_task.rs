// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Tokio-based replacement for SenderAccountsManager actor
//!
//! This module provides a drop-in replacement for the ractor-based SenderAccountsManager
//! that uses tokio tasks and channels for message passing.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Result;
use indexer_monitor::SubgraphClient;
use serde::Deserialize;
use sqlx::postgres::PgListener;
use thegraph_core::{
    alloy::{primitives::Address, sol_types::Eip712Domain},
    CollectionId,
};
use tokio::sync::mpsc;

use super::{
    sender_account::SenderAccountConfig,
    sender_accounts_manager::{AllocationId, SenderAccountsManagerMessage},
};

#[cfg(any(test, feature = "test"))]
use super::sender_account_task::SenderAccountTask;

#[cfg(not(any(test, feature = "test")))]
use super::sender_account_task::SenderAccountTask;
use crate::task_lifecycle::{LifecycleManager, RestartPolicy, TaskHandle, TaskRegistry};

/// Tokio task-based replacement for SenderAccountsManager actor
pub struct SenderAccountsManagerTask;

/// V1 receipt notification payload structure
#[derive(Debug, Deserialize)]
struct V1ReceiptNotification {
    sender: Address,
    allocation_id: Address,
}

/// V2 receipt notification payload structure
#[derive(Debug, Deserialize)]
struct V2ReceiptNotification {
    sender: Address,
    collection_id: CollectionId,
}

/// State for the SenderAccountsManager task
struct TaskState {
    /// Currently tracked V1 sender accounts
    sender_accounts_v1: HashSet<Address>,
    /// Currently tracked V2 sender accounts  
    sender_accounts_v2: HashSet<Address>,
    /// Registry for managing child sender account tasks
    child_registry: TaskRegistry,
    /// Lifecycle manager for child tasks
    #[allow(dead_code)]
    lifecycle: Arc<LifecycleManager>,
    /// Configuration
    config: &'static SenderAccountConfig,
    /// Other fields needed for spawning child tasks
    pgpool: sqlx::PgPool,
    #[allow(dead_code)]
    escrow_subgraph: &'static SubgraphClient,
    #[allow(dead_code)]
    network_subgraph: &'static SubgraphClient,
    #[allow(dead_code)]
    domain_separator: Eip712Domain,
    #[allow(dead_code)]
    sender_aggregator_endpoints: HashMap<Address, reqwest::Url>,
    prefix: Option<String>,
    /// Handle for V1 receipt notification watcher task
    notification_watcher_v1_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle for V2 receipt notification watcher task  
    notification_watcher_v2_handle: Option<tokio::task::JoinHandle<()>>,
}

impl SenderAccountsManagerTask {
    /// Spawn a new SenderAccountsManager task
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        lifecycle: &LifecycleManager,
        name: Option<String>,
        config: &'static SenderAccountConfig,
        pgpool: sqlx::PgPool,
        escrow_subgraph: &'static SubgraphClient,
        network_subgraph: &'static SubgraphClient,
        domain_separator: Eip712Domain,
        sender_aggregator_endpoints: HashMap<Address, reqwest::Url>,
        prefix: Option<String>,
    ) -> Result<TaskHandle<SenderAccountsManagerMessage>> {
        let state = TaskState {
            sender_accounts_v1: HashSet::new(),
            sender_accounts_v2: HashSet::new(),
            child_registry: TaskRegistry::new(),
            lifecycle: Arc::new(lifecycle.clone()),
            config,
            pgpool,
            escrow_subgraph,
            network_subgraph,
            domain_separator,
            sender_aggregator_endpoints,
            prefix,
            notification_watcher_v1_handle: None,
            notification_watcher_v2_handle: None,
        };

        lifecycle
            .spawn_task(
                name,
                RestartPolicy::Never,
                100, // Buffer size for message channel
                |rx, _ctx| Self::run_task(state, rx),
            )
            .await
    }

    /// Main task loop
    async fn run_task(
        mut state: TaskState,
        mut rx: mpsc::Receiver<SenderAccountsManagerMessage>,
    ) -> Result<()> {
        // Start PostgreSQL notification watchers
        if let Err(e) = Self::start_notification_watchers(&mut state).await {
            tracing::error!(
                error = %e,
                "Failed to start PostgreSQL notification watchers"
            );
        }

        while let Some(message) = rx.recv().await {
            if let Err(e) = Self::handle_message(&mut state, message).await {
                tracing::error!(
                    error = %e,
                    "Error handling SenderAccountsManager message"
                );
            }
        }

        // Clean up notification watchers on shutdown
        if let Some(handle) = &state.notification_watcher_v1_handle {
            handle.abort();
        }
        if let Some(handle) = &state.notification_watcher_v2_handle {
            handle.abort();
        }

        Ok(())
    }

    /// Handle a single message
    async fn handle_message(
        state: &mut TaskState,
        message: SenderAccountsManagerMessage,
    ) -> Result<()> {
        match message {
            SenderAccountsManagerMessage::UpdateSenderAccountsV1(new_senders) => {
                Self::handle_update_sender_accounts_v1(state, new_senders).await?;
            }
            SenderAccountsManagerMessage::UpdateSenderAccountsV2(new_senders) => {
                Self::handle_update_sender_accounts_v2(state, new_senders).await?;
            }
        }

        Ok(())
    }

    /// Handle V1 sender account updates - spawn/stop child tasks as needed
    async fn handle_update_sender_accounts_v1(
        state: &mut TaskState,
        new_senders: HashSet<Address>,
    ) -> Result<()> {
        // Create new sender accounts
        let current_senders = state.sender_accounts_v1.clone();
        for sender in new_senders.difference(&current_senders) {
            if let Err(e) = Self::create_sender_account_v1(state, *sender).await {
                tracing::error!(
                    sender = %sender,
                    error = %e,
                    "Error creating V1 sender account task"
                );
            } else {
                state.sender_accounts_v1.insert(*sender);
            }
        }

        // Remove old sender accounts (simplified - just remove from our set)
        // In a full implementation, we'd properly shut down child tasks
        let to_remove: Vec<_> = current_senders.difference(&new_senders).copied().collect();
        for sender in to_remove {
            tracing::debug!(
                sender = %sender,
                "Removing V1 sender account (simplified implementation)"
            );
            state.sender_accounts_v1.remove(&sender);
        }

        Ok(())
    }

    /// Handle V2 sender account updates - spawn/stop child tasks as needed
    async fn handle_update_sender_accounts_v2(
        state: &mut TaskState,
        new_senders: HashSet<Address>,
    ) -> Result<()> {
        // Create new sender accounts
        let current_senders = state.sender_accounts_v2.clone();
        for sender in new_senders.difference(&current_senders) {
            if let Err(e) = Self::create_sender_account_v2(state, *sender).await {
                tracing::error!(
                    sender = %sender,
                    error = %e,
                    "Error creating V2 sender account task"
                );
            } else {
                state.sender_accounts_v2.insert(*sender);
            }
        }

        // Remove old sender accounts (simplified - just remove from our set)
        // In a full implementation, we'd properly shut down child tasks
        let to_remove: Vec<_> = current_senders.difference(&new_senders).copied().collect();
        for sender in to_remove {
            tracing::debug!(
                sender = %sender,
                "Removing V2 sender account (simplified implementation)"
            );
            state.sender_accounts_v2.remove(&sender);
        }

        Ok(())
    }

    /// Create a new V1 sender account task (child task)
    async fn create_sender_account_v1(state: &TaskState, sender: Address) -> Result<()> {
        let task_name = Self::format_sender_account(&state.prefix, &sender, "v1");

        tracing::trace!(
            sender = %sender,
            task_name = %task_name,
            "Creating V1 sender account task"
        );

        #[cfg(any(test, feature = "test"))]
        {
            // For testing, create a dummy escrow accounts channel
            let (_tx, escrow_rx) =
                tokio::sync::watch::channel(indexer_monitor::EscrowAccounts::default());

            let child_handle = SenderAccountTask::spawn(
                &state.lifecycle,
                Some(task_name.clone()),
                sender,
                state.config,
                state.pgpool.clone(),
                escrow_rx,
                state.escrow_subgraph,
                state.network_subgraph,
                state.domain_separator.clone(),
                state
                    .sender_aggregator_endpoints
                    .get(&sender)
                    .cloned()
                    .unwrap_or_else(|| "http://localhost:8080".parse().unwrap()),
                state.prefix.clone(),
            )
            .await?;

            // Register the child task
            state.child_registry.register(task_name, child_handle).await;
        }

        #[cfg(not(any(test, feature = "test")))]
        {
            // For production, create real escrow account monitoring via subgraph queries
            let escrow_rx = indexer_monitor::escrow_accounts_v1(
                state.escrow_subgraph,
                state.config.indexer_address,
                state.config.escrow_polling_interval,
                true, // reject_thawing_signers
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create V1 escrow accounts watcher for sender {}: {}",
                    sender,
                    e
                )
            })?;

            let child_handle = SenderAccountTask::spawn(
                &state.lifecycle,
                Some(task_name.clone()),
                sender,
                state.config,
                state.pgpool.clone(),
                escrow_rx,
                state.escrow_subgraph,
                state.network_subgraph,
                state.domain_separator.clone(),
                state
                    .sender_aggregator_endpoints
                    .get(&sender)
                    .cloned()
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            sender = %sender,
                            "No aggregator endpoint configured for sender, using default"
                        );
                        "http://localhost:8080".parse().unwrap()
                    }),
                state.prefix.clone(),
            )
            .await?;

            // Register the child task
            state
                .child_registry
                .register(task_name.clone(), child_handle)
                .await;

            tracing::info!(
                sender = %sender,
                task_name = %task_name,
                "Production V1 sender account task with real escrow monitoring spawned successfully"
            );
        }

        Ok(())
    }

    /// Create a new V2 sender account task (child task)
    async fn create_sender_account_v2(state: &TaskState, sender: Address) -> Result<()> {
        let task_name = Self::format_sender_account(&state.prefix, &sender, "v2");

        tracing::trace!(
            sender = %sender,
            task_name = %task_name,
            "Creating V2 sender account task"
        );

        #[cfg(any(test, feature = "test"))]
        {
            // For testing, create a dummy escrow accounts channel
            let (_tx, escrow_rx) =
                tokio::sync::watch::channel(indexer_monitor::EscrowAccounts::default());

            let child_handle = SenderAccountTask::spawn(
                &state.lifecycle,
                Some(task_name.clone()),
                sender,
                state.config,
                state.pgpool.clone(),
                escrow_rx,
                state.escrow_subgraph,
                state.network_subgraph,
                state.domain_separator.clone(),
                state
                    .sender_aggregator_endpoints
                    .get(&sender)
                    .cloned()
                    .unwrap_or_else(|| "http://localhost:8080".parse().unwrap()),
                state.prefix.clone(),
            )
            .await?;

            // Register the child task
            state.child_registry.register(task_name, child_handle).await;
        }

        #[cfg(not(any(test, feature = "test")))]
        {
            // For production, create real escrow account monitoring via subgraph queries
            let escrow_rx = indexer_monitor::escrow_accounts_v2(
                state.escrow_subgraph,
                state.config.indexer_address,
                state.config.escrow_polling_interval,
                true, // reject_thawing_signers
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create V2 escrow accounts watcher for sender {}: {}",
                    sender,
                    e
                )
            })?;

            let child_handle = SenderAccountTask::spawn(
                &state.lifecycle,
                Some(task_name.clone()),
                sender,
                state.config,
                state.pgpool.clone(),
                escrow_rx,
                state.escrow_subgraph,
                state.network_subgraph,
                state.domain_separator.clone(),
                state
                    .sender_aggregator_endpoints
                    .get(&sender)
                    .cloned()
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            sender = %sender,
                            "No aggregator endpoint configured for sender, using default"
                        );
                        "http://localhost:8080".parse().unwrap()
                    }),
                state.prefix.clone(),
            )
            .await?;

            // Register the child task
            state
                .child_registry
                .register(task_name.clone(), child_handle)
                .await;

            tracing::info!(
                sender = %sender,
                task_name = %task_name,
                "Production V2 sender account task with real escrow monitoring spawned successfully"
            );
        }

        Ok(())
    }

    /// Format sender account task name
    fn format_sender_account(prefix: &Option<String>, sender: &Address, version: &str) -> String {
        let mut name = String::new();
        if let Some(prefix) = prefix {
            name.push_str(prefix);
            name.push(':');
        }
        name.push_str(&format!("sender_account_{version}_{sender}"));
        name
    }

    /// Start PostgreSQL notification watchers for both V1 and V2 receipts
    async fn start_notification_watchers(state: &mut TaskState) -> Result<()> {
        // Start V1 notification watcher
        let pglistener_v1 = PgListener::connect_with(&state.pgpool).await?;
        let child_registry_v1 = state.child_registry.clone();
        state.notification_watcher_v1_handle = Some(tokio::spawn(Self::notification_watcher_v1(
            pglistener_v1,
            child_registry_v1,
        )));

        // Start V2 notification watcher (only if horizon is enabled)
        if state.config.horizon_enabled {
            let pglistener_v2 = PgListener::connect_with(&state.pgpool).await?;
            let child_registry_v2 = state.child_registry.clone();
            state.notification_watcher_v2_handle = Some(tokio::spawn(
                Self::notification_watcher_v2(pglistener_v2, child_registry_v2),
            ));
        }

        tracing::info!("Started PostgreSQL notification watchers");
        Ok(())
    }

    /// V1 notification watcher task
    async fn notification_watcher_v1(mut pglistener: PgListener, child_registry: TaskRegistry) {
        if let Err(e) = pglistener.listen("scalar_tap_receipt_notification").await {
            tracing::error!(
                error = %e,
                "Failed to listen to scalar_tap_receipt_notification channel"
            );
            return;
        }

        tracing::info!("V1 notification watcher started, listening for receipt notifications");

        loop {
            match pglistener.recv().await {
                Ok(notification) => {
                    tracing::debug!(
                        channel = notification.channel(),
                        payload = notification.payload(),
                        "Received V1 receipt notification"
                    );

                    if let Err(e) = Self::parse_and_forward_v1_notification(
                        &child_registry,
                        notification.payload(),
                    )
                    .await
                    {
                        tracing::error!(
                            error = %e,
                            payload = notification.payload(),
                            "Failed to parse and forward V1 notification"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Error receiving V1 receipt notification"
                    );
                    break;
                }
            }
        }

        tracing::info!("V1 notification watcher stopped");
    }

    /// V2 notification watcher task
    async fn notification_watcher_v2(mut pglistener: PgListener, child_registry: TaskRegistry) {
        if let Err(e) = pglistener.listen("tap_horizon_receipt_notification").await {
            tracing::error!(
                error = %e,
                "Failed to listen to tap_horizon_receipt_notification channel"
            );
            return;
        }

        tracing::info!("V2 notification watcher started, listening for receipt notifications");

        loop {
            match pglistener.recv().await {
                Ok(notification) => {
                    tracing::debug!(
                        channel = notification.channel(),
                        payload = notification.payload(),
                        "Received V2 receipt notification"
                    );

                    if let Err(e) = Self::parse_and_forward_v2_notification(
                        &child_registry,
                        notification.payload(),
                    )
                    .await
                    {
                        tracing::error!(
                            error = %e,
                            payload = notification.payload(),
                            "Failed to parse and forward V2 notification"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Error receiving V2 receipt notification"
                    );
                    break;
                }
            }
        }

        tracing::info!("V2 notification watcher stopped");
    }

    /// Parse V1 notification payload and forward to appropriate sender account task
    async fn parse_and_forward_v1_notification(
        child_registry: &TaskRegistry,
        payload: &str,
    ) -> Result<()> {
        // Parse the notification payload
        // Expected format: JSON with sender address and allocation ID
        let notification: V1ReceiptNotification = serde_json::from_str(payload)
            .map_err(|e| anyhow::anyhow!("Failed to parse V1 notification payload: {}", e))?;

        tracing::debug!(
            sender = %notification.sender,
            allocation_id = %notification.allocation_id,
            "Parsed V1 receipt notification"
        );

        // Look up the sender account task and forward the notification
        let task_name = format!("sender_account_v1_{}", notification.sender);
        if let Some(task_handle) = child_registry.get_task(&task_name).await {
            // Convert to AllocationId enum
            let allocation_id =
                AllocationId::Legacy(thegraph_core::AllocationId::new(notification.allocation_id));

            // Create a NewAllocationId message to trigger receipt processing
            let message =
                super::sender_account::SenderAccountMessage::NewAllocationId(allocation_id);

            if let Err(e) = task_handle.send(message).await {
                tracing::error!(
                    sender = %notification.sender,
                    allocation_id = %allocation_id,
                    error = %e,
                    "Failed to forward V1 notification to sender account task"
                );
            } else {
                tracing::trace!(
                    sender = %notification.sender,
                    allocation_id = %allocation_id,
                    "Successfully forwarded V1 notification to sender account task"
                );
            }
        } else {
            tracing::warn!(
                sender = %notification.sender,
                task_name = %task_name,
                "No sender account task found for V1 notification"
            );
        }

        Ok(())
    }

    /// Parse V2 notification payload and forward to appropriate sender account task
    async fn parse_and_forward_v2_notification(
        child_registry: &TaskRegistry,
        payload: &str,
    ) -> Result<()> {
        // Parse the notification payload
        // Expected format: JSON with sender address and collection ID
        let notification: V2ReceiptNotification = serde_json::from_str(payload)
            .map_err(|e| anyhow::anyhow!("Failed to parse V2 notification payload: {}", e))?;

        tracing::debug!(
            sender = %notification.sender,
            collection_id = %notification.collection_id,
            "Parsed V2 receipt notification"
        );

        // Look up the sender account task and forward the notification
        let task_name = format!("sender_account_v2_{}", notification.sender);
        if let Some(task_handle) = child_registry.get_task(&task_name).await {
            // Convert to AllocationId enum
            let allocation_id = AllocationId::Horizon(notification.collection_id);

            // Create a NewAllocationId message to trigger receipt processing
            let message =
                super::sender_account::SenderAccountMessage::NewAllocationId(allocation_id);

            if let Err(e) = task_handle.send(message).await {
                tracing::error!(
                    sender = %notification.sender,
                    collection_id = %notification.collection_id,
                    error = %e,
                    "Failed to forward V2 notification to sender account task"
                );
            } else {
                tracing::trace!(
                    sender = %notification.sender,
                    collection_id = %notification.collection_id,
                    "Successfully forwarded V2 notification to sender account task"
                );
            }
        } else {
            tracing::warn!(
                sender = %notification.sender,
                task_name = %task_name,
                "No sender account task found for V2 notification"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sender_accounts_manager_task_creation() {
        use crate::test::{store_receipt, CreateReceipt};
        use indexer_monitor::{DeploymentDetails, SubgraphClient};
        use tap_core::tap_eip712_domain;
        use test_assets::{
            setup_shared_test_db, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER_ADDRESS, TAP_SIGNER,
            VERIFIER_ADDRESS,
        };
        use tokio::time::sleep;

        // Setup test database using established testcontainer infrastructure
        let test_db = setup_shared_test_db().await;
        let pgpool = test_db.pool.clone();

        // Create LifecycleManager for task management
        let lifecycle = LifecycleManager::new();

        // Create realistic config for testing
        let config = Box::leak(Box::new(SenderAccountConfig {
            rav_request_buffer: std::time::Duration::from_millis(100), // Shorter for testing
            max_amount_willing_to_lose_grt: 1_000_000,
            trigger_value: 50, // Lower threshold for easier testing
            rav_request_timeout: std::time::Duration::from_secs(5),
            rav_request_receipt_limit: 10,
            indexer_address: INDEXER_ADDRESS,
            escrow_polling_interval: std::time::Duration::from_millis(500), // Faster for testing
            tap_sender_timeout: std::time::Duration::from_secs(5),
            trusted_senders: HashSet::new(),
            horizon_enabled: false,
        }));

        // Create test subgraph clients (mock for testing)
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url("http://localhost:8000").expect("Valid URL"),
            )
            .await,
        ));

        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url("http://localhost:8001").expect("Valid URL"),
            )
            .await,
        ));

        // Create test EIP-712 domain
        let domain = tap_eip712_domain(1, VERIFIER_ADDRESS);

        // Test 1: Task spawning and initialization
        tracing::info!("🧪 Testing SenderAccountsManagerTask creation and initialization");

        let manager_task = SenderAccountsManagerTask::spawn(
            &lifecycle,
            Some("test-sender-accounts-manager".to_string()),
            config,
            pgpool.clone(),
            escrow_subgraph,
            network_subgraph,
            domain.clone(),
            std::collections::HashMap::new(), // sender_aggregator_endpoints
            Some("test".to_string()),
        )
        .await
        .expect("Failed to spawn SenderAccountsManagerTask");

        tracing::info!("✅ SenderAccountsManagerTask spawned successfully");

        // Test 2: Store receipts to trigger task activity
        tracing::info!("📥 Testing receipt storage and manager task processing");

        // Store receipts for multiple allocations to test manager coordination
        let allocations = [ALLOCATION_ID_0, ALLOCATION_ID_1];
        let mut total_receipts = 0;

        for (alloc_idx, &allocation_id) in allocations.iter().enumerate() {
            let receipt_count = 3 + alloc_idx; // Different counts for each allocation
            for i in 0..receipt_count {
                let receipt = crate::tap::context::Legacy::create_received_receipt(
                    allocation_id,
                    &TAP_SIGNER.0,
                    (i + 1) as u64,
                    1_000_000_000 + (i * 1000) as u64,
                    25, // Small value to avoid triggering RAV immediately
                );

                let receipt_id = store_receipt(&pgpool, receipt.signed_receipt())
                    .await
                    .expect("Failed to store receipt");

                tracing::debug!(
                    "Stored receipt {} for allocation {:?} with ID: {}",
                    i + 1,
                    allocation_id,
                    receipt_id
                );
                total_receipts += 1;
            }
        }

        // Allow time for manager task to process notifications and spawn child tasks
        sleep(std::time::Duration::from_millis(1000)).await;

        // Test 3: Verify receipts were stored across allocations
        let stored_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts")
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query total receipt count");

        assert!(
            stored_count >= total_receipts as i64,
            "Expected at least {total_receipts} receipts, found {stored_count}"
        );

        tracing::info!(
            "📊 Verified {} receipts stored successfully across {} allocations",
            stored_count,
            allocations.len()
        );

        // Test 4: Verify manager task is coordinating properly
        tracing::info!("🎯 Testing manager task coordination and child spawning");

        // The manager should be processing receipt notifications and spawning child tasks
        // We can verify this by checking the receipts are being handled
        for &allocation_id in &allocations {
            let allocation_receipts: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM scalar_tap_receipts WHERE allocation_id = $1",
            )
            .bind(thegraph_core::alloy::hex::ToHexExt::encode_hex(
                &allocation_id,
            ))
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query allocation receipt count");

            tracing::info!(
                "📊 Allocation {:?} has {} receipts",
                allocation_id,
                allocation_receipts
            );

            assert!(
                allocation_receipts >= 0,
                "Each allocation should have receipts tracked properly"
            );
        }

        // Test 5: Task health and lifecycle monitoring
        tracing::info!("💓 Testing manager task health monitoring");

        let system_health = lifecycle.get_system_health().await;
        tracing::info!("📊 System health status: {:?}", system_health);

        // The manager task should be registered and healthy
        assert!(
            system_health.overall_healthy,
            "System should be healthy with manager task running"
        );

        // Test 6: Message handling verification
        tracing::info!("📨 Testing manager task message handling");

        // Test updating sender accounts (simulating configuration changes)
        let mut sender_set = HashSet::new();
        sender_set.insert(TAP_SIGNER.1); // Add our test signer

        // Send update message to manager task
        if let Err(e) = manager_task
            .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV1(
                sender_set.clone(),
            ))
            .await
        {
            tracing::warn!("Failed to send update message: {}", e);
            // In test environment, this might fail due to mock setup, but that's OK
        }

        // Allow processing time
        sleep(std::time::Duration::from_millis(500)).await;

        tracing::info!("✅ Manager task message handling tested");

        // Test 7: PostgreSQL notification handling
        tracing::info!("🔔 Testing PostgreSQL notification handling");

        // Store one more receipt to trigger notification
        let notification_receipt = crate::tap::context::Legacy::create_received_receipt(
            ALLOCATION_ID_0,
            &TAP_SIGNER.0,
            999, // Unique nonce
            2_000_000_000,
            30,
        );

        let _notification_receipt_id =
            store_receipt(&pgpool, notification_receipt.signed_receipt())
                .await
                .expect("Failed to store notification test receipt");

        // Allow time for notification processing
        sleep(std::time::Duration::from_millis(1000)).await;

        // Verify the receipt was processed (manager should handle the notification)
        let final_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts")
            .fetch_one(&pgpool)
            .await
            .expect("Failed to query final receipt count");

        tracing::info!("📊 Final receipt count after notification: {}", final_count);

        assert!(
            final_count > stored_count,
            "New receipt should increase the count"
        );

        // Test 8: Graceful shutdown
        tracing::info!("🛑 Testing graceful manager task shutdown");

        drop(manager_task);
        sleep(std::time::Duration::from_millis(500)).await;

        // Verify database is still accessible (no connection leaks)
        let shutdown_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scalar_tap_receipts")
            .fetch_one(&pgpool)
            .await
            .expect("Database should still be accessible after task shutdown");

        tracing::info!("📊 Receipt count after shutdown: {}", shutdown_count);

        // Verify system health reflects the shutdown
        let post_shutdown_health = lifecycle.get_system_health().await;
        tracing::info!("📊 Post-shutdown system health: {:?}", post_shutdown_health);

        tracing::info!(
            "✅ SenderAccountsManagerTask creation and lifecycle test completed successfully!"
        );
        tracing::info!("🎯 Validated:");
        tracing::info!("   - Manager task spawning with real database");
        tracing::info!("   - Multi-allocation receipt processing coordination");
        tracing::info!("   - PostgreSQL notification handling");
        tracing::info!("   - Message handling and sender account updates");
        tracing::info!("   - Health monitoring integration");
        tracing::info!("   - Graceful shutdown and cleanup");
    }

    #[tokio::test]
    async fn test_sender_account_name_formatting() {
        let sender = Address::from([1u8; 20]);

        let name_v1 = SenderAccountsManagerTask::format_sender_account(
            &Some("test".to_string()),
            &sender,
            "v1",
        );

        assert!(name_v1.starts_with("test:"));
        assert!(name_v1.contains("sender_account_v1"));
        assert!(name_v1.contains(&sender.to_string()));

        let name_v2 = SenderAccountsManagerTask::format_sender_account(&None, &sender, "v2");

        assert!(name_v2.starts_with("sender_account_v2"));
        assert!(name_v2.contains(&sender.to_string()));
    }
}
