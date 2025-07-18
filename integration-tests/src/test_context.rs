// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Test Context and Infrastructure
//!
//! This module provides the foundation for structured integration testing,
//! including test isolation, proper setup/teardown, and reusable utilities.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use reqwest::Client;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

use crate::{constants::*, metrics::MetricsChecker};

/// Test context providing isolated environment for integration tests
pub struct TestContext {
    /// Unique identifier for this test run
    pub test_id: String,
    /// HTTP client for making requests
    pub http_client: Arc<Client>,
    /// Metrics checker for monitoring system state
    pub metrics_checker: MetricsChecker,
    /// Test wallet for signing transactions
    pub wallet: PrivateKeySigner,
    /// Cleanup tasks to run when test completes
    pub cleanup_tasks: Vec<CleanupTask>,
    /// Test-specific allocations
    pub allocations: Vec<TestAllocation>,
    /// Escrow account states - reserved for future allocation lifecycle tests
    #[allow(dead_code)]
    pub escrow_accounts: HashMap<Address, TestEscrowAccount>,
}

/// Represents an allocation used in testing
///
/// Contains comprehensive allocation metadata for future allocation lifecycle tests.
/// Currently only `id` is used by basic receipt tests.
#[derive(Debug, Clone)]
pub struct TestAllocation {
    pub id: Address,
    /// Indexer address - reserved for future allocation management tests
    #[allow(dead_code)]
    pub indexer: Address,
    /// Subgraph deployment ID - reserved for future deployment-specific tests
    #[allow(dead_code)]
    pub subgraph_deployment: String,
    /// Allocation status - reserved for future allocation lifecycle tests
    #[allow(dead_code)]
    pub status: AllocationStatus,
    /// Block number when allocation was created - reserved for future time-based tests
    #[allow(dead_code)]
    pub created_at_block: u64,
}

/// Allocation status tracking for future allocation lifecycle tests
///
/// Currently only `Active` is used. `Closed` and `Finalized` are reserved for
/// comprehensive allocation lifecycle testing including allocation closing and RAV redemption.
#[derive(Debug, Clone, PartialEq)]
pub enum AllocationStatus {
    Active,
    /// Reserved for allocation closing tests
    #[allow(dead_code)]
    Closed,
    /// Reserved for RAV redemption tests
    #[allow(dead_code)]
    Finalized,
}

/// Test escrow account state for future escrow balance testing
///
/// Reserved for comprehensive escrow balance tracking and validation tests.
/// Currently not used by basic receipt tests.
#[derive(Debug, Clone)]
pub struct TestEscrowAccount {
    /// Escrow account address
    #[allow(dead_code)]
    pub address: Address,
    /// V1 (Legacy) escrow balance
    #[allow(dead_code)]
    pub balance_v1: u128,
    /// V2 (Horizon) escrow balance
    #[allow(dead_code)]
    pub balance_v2: u128,
    /// Last time balance was updated
    #[allow(dead_code)]
    pub last_updated: SystemTime,
}

/// Cleanup task to run after test completion
///
/// Reserved for comprehensive cleanup in allocation lifecycle tests.
/// Currently not used by basic receipt tests which don't require cleanup.
pub enum CleanupTask {
    /// Reserved for allocation closing tests
    #[allow(dead_code)]
    CloseAllocation(Address),
    /// Reserved for escrow cleanup tests
    #[allow(dead_code)]
    RemoveEscrowFunds(Address),
    /// Reserved for metrics reset tests
    #[allow(dead_code)]
    ResetMetrics,
}

/// Structured test errors for better diagnostics
#[derive(Debug, thiserror::Error)]
pub enum TestError {
    /// Reserved for escrow balance validation tests
    #[allow(dead_code)]
    #[error("Escrow insufficient: sender={sender}, required={required}, available={available}")]
    EscrowInsufficient {
        sender: Address,
        required: u128,
        available: u128,
    },

    #[error(
        "Horizon detection failed: expected {expected_accounts} accounts, found {found_accounts}"
    )]
    HorizonDetectionFailed {
        expected_accounts: usize,
        found_accounts: usize,
    },

    #[error("Receipt validation failed: {reason}")]
    ReceiptValidationFailed { reason: String },

    #[error("Service unavailable: {service}")]
    ServiceUnavailable { service: String },

    #[error("Timeout waiting for condition: {condition}")]
    Timeout { condition: String },

    #[error("Allocation not found: {allocation_id}")]
    AllocationNotFound { allocation_id: Address },

    #[error("Test setup failed: {reason}")]
    SetupFailed { reason: String },
}

impl TestContext {
    /// Create a new test context with isolated environment
    pub async fn new() -> Result<Self> {
        let test_id = format!(
            "test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        let http_client = Arc::new(Client::new());
        let metrics_checker =
            MetricsChecker::new(http_client.clone(), TAP_AGENT_METRICS_URL.to_string());
        let wallet: PrivateKeySigner =
            ACCOUNT0_SECRET
                .parse()
                .map_err(|e| TestError::SetupFailed {
                    reason: format!("Invalid wallet key: {e}"),
                })?;

        Ok(Self {
            test_id,
            http_client,
            metrics_checker,
            wallet,
            cleanup_tasks: Vec::new(),
            allocations: Vec::new(),
            escrow_accounts: HashMap::new(),
        })
    }

    /// Add a cleanup task to run after test completion
    ///
    /// Reserved for future allocation lifecycle tests that need cleanup.
    #[allow(dead_code)]
    pub fn add_cleanup_task(&mut self, task: CleanupTask) {
        self.cleanup_tasks.push(task);
    }

    /// Find an active allocation for testing
    pub async fn find_active_allocation(&self) -> Result<TestAllocation> {
        let response = self.http_client
            .post(GRAPH_URL)
            .json(&serde_json::json!({
                "query": "{ allocations(where: { status: Active }) { id indexer { id } subgraphDeployment { id } } }"
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(TestError::ServiceUnavailable {
                service: "Network subgraph".to_string(),
            }));
        }

        let response_text = response.text().await?;
        let json_value = serde_json::from_str::<serde_json::Value>(&response_text)?;

        let allocation_data = json_value
            .get("data")
            .and_then(|d| d.get("allocations"))
            .and_then(|a| a.as_array())
            .and_then(|arr| arr.first())
            .ok_or(TestError::AllocationNotFound {
                allocation_id: Address::ZERO,
            })?;

        let allocation_id = allocation_data.get("id").and_then(|id| id.as_str()).ok_or(
            TestError::AllocationNotFound {
                allocation_id: Address::ZERO,
            },
        )?;

        let indexer_id = allocation_data
            .get("indexer")
            .and_then(|i| i.get("id"))
            .and_then(|id| id.as_str())
            .ok_or(TestError::AllocationNotFound {
                allocation_id: Address::ZERO,
            })?;

        let subgraph_deployment = allocation_data
            .get("subgraphDeployment")
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
            .ok_or(TestError::AllocationNotFound {
                allocation_id: Address::ZERO,
            })?;

        Ok(TestAllocation {
            id: allocation_id.parse()?,
            indexer: indexer_id.parse()?,
            subgraph_deployment: subgraph_deployment.to_string(),
            status: AllocationStatus::Active,
            created_at_block: 0, // TODO: Get from subgraph
        })
    }

    /// Check if Horizon contracts are detected
    pub async fn verify_horizon_detection(&self) -> Result<bool> {
        let response = self
            .http_client
            .post(GRAPH_URL)
            .json(&serde_json::json!({
                "query": "{ paymentsEscrowAccounts(first: 1) { id } }"
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let response_text = response.text().await?;
        let json_value = serde_json::from_str::<serde_json::Value>(&response_text)?;

        let accounts = json_value
            .get("data")
            .and_then(|d| d.get("paymentsEscrowAccounts"))
            .and_then(|a| a.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);

        Ok(accounts > 0)
    }

    /// Wait for a condition to be met with timeout
    ///
    /// Reserved for future complex condition waiting tests.
    #[allow(dead_code)]
    pub async fn wait_for_condition<F>(
        &self,
        condition: F,
        timeout: Duration,
        description: &str,
    ) -> Result<()>
    where
        F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send>>,
    {
        let start = SystemTime::now();

        while start.elapsed().unwrap() < timeout {
            if condition().await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Err(anyhow::anyhow!(TestError::Timeout {
            condition: description.to_string(),
        }))
    }

    /// Get current escrow balance for an address
    ///
    /// Reserved for future escrow balance tracking tests.
    #[allow(dead_code)]
    pub async fn get_escrow_balance(&self, address: Address) -> Result<TestEscrowAccount> {
        // This would check both V1 and V2 escrow balances
        // For now, return a placeholder
        Ok(TestEscrowAccount {
            address,
            balance_v1: 0,
            balance_v2: 0,
            last_updated: SystemTime::now(),
        })
    }

    /// Execute all cleanup tasks
    pub async fn cleanup(&mut self) -> Result<()> {
        println!("🧹 Cleaning up test context: {}", self.test_id);

        for task in &self.cleanup_tasks {
            match task {
                CleanupTask::CloseAllocation(allocation_id) => {
                    println!("  - Closing allocation: {allocation_id}");
                    // TODO: Implement allocation closing
                }
                CleanupTask::RemoveEscrowFunds(address) => {
                    println!("  - Removing escrow funds for: {address}");
                    // TODO: Implement escrow cleanup
                }
                CleanupTask::ResetMetrics => {
                    println!("  - Resetting metrics");
                    // TODO: Implement metrics reset
                }
            }
        }

        self.cleanup_tasks.clear();
        Ok(())
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        if !self.cleanup_tasks.is_empty() {
            println!(
                "⚠️  Test context dropped with {} pending cleanup tasks",
                self.cleanup_tasks.len()
            );
        }
    }
}

/// Test result wrapper with additional diagnostics
///
/// Reserved for future advanced test result tracking and metrics collection.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TestResult<T> {
    pub result: Result<T>,
    pub metrics_snapshot: Option<serde_json::Value>,
    pub duration: Duration,
    pub test_id: String,
}

impl<T> TestResult<T> {
    /// Reserved for future advanced test result tracking
    #[allow(dead_code)]
    pub fn new(result: Result<T>, test_id: String, duration: Duration) -> Self {
        Self {
            result,
            metrics_snapshot: None,
            duration,
            test_id,
        }
    }

    /// Reserved for future metrics integration
    #[allow(dead_code)]
    pub fn with_metrics(mut self, metrics: serde_json::Value) -> Self {
        self.metrics_snapshot = Some(metrics);
        self
    }
}

/// Macro for running tests with proper context and cleanup
#[macro_export]
macro_rules! test_with_context {
    ($test_name:ident, $test_body:expr) => {
        #[tokio::test]
        async fn $test_name() {
            let start = std::time::SystemTime::now();
            let mut ctx = TestContext::new()
                .await
                .expect("Failed to create test context");

            println!(
                "🧪 Starting test: {} (ID: {})",
                stringify!($test_name),
                ctx.test_id
            );

            let result = { $test_body(&mut ctx).await };
            let duration = start.elapsed().unwrap();

            // Cleanup
            if let Err(e) = ctx.cleanup().await {
                eprintln!("⚠️  Cleanup failed: {}", e);
            }

            println!(
                "✅ Test completed: {} in {:?}",
                stringify!($test_name),
                duration
            );

            result.expect("Test failed");
        }
    };
}
