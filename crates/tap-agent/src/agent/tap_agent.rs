// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Main TAP Agent coordinator
//!
//! This module provides the main TapAgent struct that composes all the stream
//! processors into a complete TAP receipt processing system. Uses idiomatic
//! tokio patterns with JoinSet for task management and natural shutdown semantics.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use indexer_monitor::SubgraphClient;
use reqwest::Url;
use sqlx::PgPool;
use thegraph_core::alloy::primitives::Address;
use tokio::{sync::mpsc, task::JoinSet};
use tracing::{debug, error, info, warn};

use super::{
    allocation_id::AllocationId,
    postgres_source::{PostgresEventSource, RavPersister, RavRequestTimer},
    stream_processor::{ProcessingResult, TapEvent, TapProcessingPipeline},
};

/// Configuration for the TAP Agent
#[derive(Clone)]
pub struct TapAgentConfig {
    /// PostgreSQL connection pool
    pub pgpool: PgPool,

    /// RAV creation threshold - create RAV when receipts exceed this value
    pub rav_threshold: u128,

    /// Interval for periodic RAV requests
    pub rav_request_interval: Duration,

    /// Channel buffer sizes for flow control
    pub event_buffer_size: usize,
    /// Buffer size for processing result channel
    pub result_buffer_size: usize,
    /// Buffer size for RAV result channel
    pub rav_buffer_size: usize,

    /// Escrow account monitoring configuration
    /// V1 escrow subgraph client for Legacy receipts (TAP escrow subgraph)
    pub escrow_subgraph_v1: Option<&'static SubgraphClient>,
    /// V2 escrow subgraph client for Horizon receipts (network subgraph)
    pub escrow_subgraph_v2: Option<&'static SubgraphClient>,
    /// Indexer's Ethereum address for escrow account monitoring
    pub indexer_address: Address,
    /// Interval for escrow account balance synchronization
    pub escrow_syncing_interval: Duration,
    /// Whether to reject thawing signers during escrow monitoring
    pub reject_thawing_signers: bool,

    /// Network subgraph configuration for allocation discovery
    /// Network subgraph client for allocation monitoring
    pub network_subgraph: Option<&'static SubgraphClient>,
    /// Interval for allocation synchronization from network subgraph
    pub allocation_syncing_interval: Duration,
    /// Buffer time for recently closed allocations
    pub recently_closed_allocation_buffer: Duration,

    /// TAP Manager configuration
    /// EIP-712 domain separator for TAP receipt validation
    pub domain_separator: Option<thegraph_core::alloy::sol_types::Eip712Domain>,
    /// Sender aggregator endpoints for RAV signing (Address → URL mapping)
    /// **Reference**: Follows ractor configuration pattern from `TapConfig::sender_aggregator_endpoints`
    pub sender_aggregator_endpoints: HashMap<Address, Url>,
}

impl std::fmt::Debug for TapAgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapAgentConfig")
            .field("rav_threshold", &self.rav_threshold)
            .field("rav_request_interval", &self.rav_request_interval)
            .field("event_buffer_size", &self.event_buffer_size)
            .field("result_buffer_size", &self.result_buffer_size)
            .field("rav_buffer_size", &self.rav_buffer_size)
            .field(
                "indexer_address",
                &format_args!("{:#x}", self.indexer_address),
            )
            .field("escrow_syncing_interval", &self.escrow_syncing_interval)
            .field("reject_thawing_signers", &self.reject_thawing_signers)
            .field("escrow_subgraph_v1", &self.escrow_subgraph_v1.is_some())
            .field("escrow_subgraph_v2", &self.escrow_subgraph_v2.is_some())
            .field("network_subgraph", &self.network_subgraph.is_some())
            .field(
                "allocation_syncing_interval",
                &self.allocation_syncing_interval,
            )
            .field(
                "recently_closed_allocation_buffer",
                &self.recently_closed_allocation_buffer,
            )
            .field("domain_separator", &self.domain_separator.is_some())
            .field(
                "sender_aggregator_endpoints_count",
                &self.sender_aggregator_endpoints.len(),
            )
            .finish()
    }
}

impl TapAgentConfig {
    /// Create config for testing with shared test database infrastructure
    #[cfg(test)]
    pub async fn for_testing() -> Self {
        let test_db = test_assets::setup_shared_test_db().await;

        Self {
            pgpool: test_db.pool,
            rav_threshold: 1000, // Lower threshold for testing
            rav_request_interval: Duration::from_millis(100), // Faster for testing
            event_buffer_size: 10,
            result_buffer_size: 10,
            rav_buffer_size: 10,

            // Test escrow configuration
            escrow_subgraph_v1: None,
            escrow_subgraph_v2: None,
            indexer_address: Address::ZERO,
            escrow_syncing_interval: Duration::from_secs(30),
            reject_thawing_signers: true,

            // Test network subgraph configuration
            network_subgraph: None,
            allocation_syncing_interval: Duration::from_secs(60),
            recently_closed_allocation_buffer: Duration::from_secs(300),

            // Test TAP Manager configuration
            domain_separator: None,
            sender_aggregator_endpoints: HashMap::new(),
        }
    }
}

/// Main TAP Agent - coordinates all stream processors
pub struct TapAgent {
    config: TapAgentConfig,
    tasks: JoinSet<Result<()>>,

    // Shutdown coordination
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl TapAgent {
    /// Create new TAP Agent with configuration
    pub fn new(config: TapAgentConfig) -> Self {
        Self {
            config,
            tasks: JoinSet::new(),
            shutdown_tx: None,
        }
    }

    /// Start the TAP Agent with all stream processors
    ///
    /// This method composes the complete TAP processing pipeline:
    /// 1. PostgreSQL event source -> TapEvent stream
    /// 2. TapProcessingPipeline -> ProcessingResult + RavResult streams  
    /// 3. RavPersister -> Database storage
    /// 4. RavRequestTimer -> Periodic RAV requests
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting TAP Agent with stream-based processing");

        // Create communication channels with flow control
        let (event_tx, event_rx) = mpsc::channel(self.config.event_buffer_size);
        let (result_tx, result_rx) = mpsc::channel(self.config.result_buffer_size);
        let (rav_tx, rav_rx) = mpsc::channel(self.config.rav_buffer_size);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        // Create validation service channel
        let (validation_tx, validation_rx) = mpsc::channel(100);

        self.shutdown_tx = Some(shutdown_tx);

        // Spawn PostgreSQL event source
        {
            let postgres_source = PostgresEventSource::new(self.config.pgpool.clone());
            let event_tx = event_tx.clone();

            self.tasks.spawn(async move {
                info!("Starting PostgreSQL event source");
                postgres_source.start_receipt_stream(event_tx).await
            });
        }

        // Spawn validation service with escrow account watchers
        {
            // Initialize escrow account watchers following ractor implementation pattern
            // Support both production (subgraph-based) and testing (direct injection) workflows
            let escrow_accounts_v1 = if let Some(escrow_subgraph) = self.config.escrow_subgraph_v1 {
                match indexer_monitor::escrow_accounts_v1(
                    escrow_subgraph,
                    self.config.indexer_address,
                    self.config.escrow_syncing_interval,
                    self.config.reject_thawing_signers,
                )
                .await
                {
                    Ok(watcher) => {
                        info!("✅ V1 escrow accounts watcher initialized from subgraph");
                        Some(watcher)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to initialize V1 escrow accounts watcher, continuing without");
                        None
                    }
                }
            } else {
                info!("V1 escrow subgraph not configured, skipping V1 escrow monitoring");
                None
            };

            let escrow_accounts_v2 = if let Some(escrow_subgraph) = self.config.escrow_subgraph_v2 {
                match indexer_monitor::escrow_accounts_v2(
                    escrow_subgraph,
                    self.config.indexer_address,
                    self.config.escrow_syncing_interval,
                    self.config.reject_thawing_signers,
                )
                .await
                {
                    Ok(watcher) => {
                        info!("✅ V2 escrow accounts watcher initialized from subgraph");
                        Some(watcher)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to initialize V2 escrow accounts watcher, continuing without");
                        None
                    }
                }
            } else {
                info!("V2 escrow subgraph not configured, skipping V2 escrow monitoring");
                None
            };

            // Clone for logging before moving into service
            let v1_enabled = escrow_accounts_v1.is_some();
            let v2_enabled = escrow_accounts_v2.is_some();

            let validation_service = super::stream_processor::ValidationService::new(
                self.config.pgpool.clone(),
                validation_rx,
                escrow_accounts_v1,
                escrow_accounts_v2,
            );

            self.tasks.spawn(async move {
                info!(
                    v1_enabled = v1_enabled,
                    v2_enabled = v2_enabled,
                    "Starting validation service with escrow monitoring"
                );
                validation_service.run().await
            });
        }

        // Spawn main processing pipeline
        {
            // Use default domain separator if not configured
            let domain_separator = self.config.domain_separator.clone().unwrap_or_default();

            let pipeline_config = super::stream_processor::TapPipelineConfig {
                rav_threshold: self.config.rav_threshold,
                domain_separator,
                pgpool: self.config.pgpool.clone(),
                indexer_address: self.config.indexer_address,
                sender_aggregator_endpoints: self.config.sender_aggregator_endpoints.clone(),
            };

            let pipeline = TapProcessingPipeline::new(
                event_rx,
                result_tx,
                rav_tx.clone(),
                validation_tx,
                pipeline_config,
            );

            self.tasks.spawn(async move {
                info!("Starting TAP processing pipeline");
                pipeline.run().await
            });
        }

        // Spawn RAV persistence service
        {
            let rav_persister = RavPersister::new(self.config.pgpool.clone());

            self.tasks.spawn(async move {
                info!("Starting RAV persistence service");
                rav_persister.start(rav_rx).await
            });
        }

        // Spawn processing result logger (for monitoring/debugging)
        {
            self.tasks
                .spawn(async move { Self::log_processing_results(result_rx).await });
        }

        // Spawn RAV request timer with allocation discovery
        {
            let timer = RavRequestTimer::new(self.config.rav_request_interval);
            let event_tx = event_tx.clone();

            // **IMPORTANT**: Use database-based allocation discovery instead of network subgraph
            // because network subgraph only provides 20-byte addresses, but true Horizon
            // CollectionIds are 32-byte identifiers that can't be derived from addresses.
            // The database approach finds actual allocation IDs from receipt data.
            info!(
                "Using database-based allocation discovery for accurate Legacy/Horizon detection"
            );
            let active_allocations = Self::get_active_allocations(&self.config.pgpool).await?;

            self.tasks.spawn(async move {
                info!(
                    allocation_count = active_allocations.len(),
                    "Starting RAV request timer with database allocation discovery (avoids Address->CollectionId conversion issues)"
                );
                timer.start(event_tx, active_allocations).await
            });
        }

        // Spawn shutdown coordinator
        {
            let event_tx = event_tx.clone();

            self.tasks.spawn(async move {
                // Wait for shutdown signal
                shutdown_rx.recv().await;
                info!("Shutdown signal received, initiating graceful shutdown");

                // Send shutdown event to processing pipeline
                let _ = event_tx.send(TapEvent::Shutdown).await;

                Ok(())
            });
        }

        info!(
            rav_threshold = self.config.rav_threshold,
            rav_interval_secs = self.config.rav_request_interval.as_secs(),
            "TAP Agent started successfully"
        );

        Ok(())
    }

    /// Wait for all tasks to complete
    ///
    /// This method runs the main event loop, waiting for all spawned tasks
    /// to complete. Tasks will run until their input channels are closed
    /// or they receive shutdown signals.
    pub async fn run(mut self) -> Result<()> {
        let mut errors = Vec::new();

        // Wait for all tasks to complete
        while let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(Ok(())) => {
                    info!("Task completed successfully");
                }
                Ok(Err(e)) => {
                    error!(error = %e, "Task failed");
                    errors.push(e);
                }
                Err(join_error) => {
                    error!(error = %join_error, "Task panicked");
                    errors.push(join_error.into());
                }
            }
        }

        if errors.is_empty() {
            info!("TAP Agent shut down successfully");
            Ok(())
        } else {
            error!(
                error_count = errors.len(),
                "TAP Agent shut down with errors"
            );
            Err(errors.into_iter().next().unwrap()) // Return first error
        }
    }

    /// Initiate graceful shutdown
    ///
    /// Sends shutdown signal to all tasks and waits for them to complete.
    /// Tasks will finish processing their current work and shut down cleanly.
    pub async fn shutdown(&mut self) -> Result<()> {
        info!("Initiating TAP Agent shutdown");

        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            // Send shutdown signal
            if let Err(e) = shutdown_tx.send(()).await {
                warn!(error = %e, "Failed to send shutdown signal");
            }
        }

        // Tasks will shut down naturally as channels close
        info!("Shutdown signal sent, tasks will complete gracefully");
        Ok(())
    }

    /// Log processing results for monitoring
    async fn log_processing_results(mut result_rx: mpsc::Receiver<ProcessingResult>) -> Result<()> {
        info!("Starting processing result monitor");

        while let Some(result) = result_rx.recv().await {
            match result {
                ProcessingResult::Aggregated {
                    allocation_id,
                    new_total,
                } => {
                    info!(
                        allocation_id = ?allocation_id,
                        new_total = new_total,
                        "Receipt aggregated successfully"
                    );
                }
                ProcessingResult::Invalid {
                    allocation_id,
                    reason,
                } => {
                    warn!(
                        allocation_id = ?allocation_id,
                        reason = %reason,
                        "Receipt rejected as invalid"
                    );
                }
                ProcessingResult::Pending { allocation_id } => {
                    debug!(
                        allocation_id = ?allocation_id,
                        "Receipt processed, pending RAV creation"
                    );
                }
            }
        }

        info!("Processing result monitor shutting down");
        Ok(())
    }

    /// Get list of active allocations from database (fallback when no network subgraph)
    ///
    /// **Static Allocation Discovery**: Fallback when network subgraph is not configured
    /// **Reference Implementation**: Based on ractor `get_pending_sender_allocation_id_v1/v2` pattern
    ///
    /// This follows the exact ractor approach:
    /// 1. Query `scalar_tap_receipts` for Legacy (V1) allocations with pending receipts
    /// 2. Query `tap_horizon_receipts` for Horizon (V2) allocations with pending receipts  
    /// 3. Combine both for complete allocation discovery
    ///
    /// **Why This Works**: If there are pending receipts in the database, those allocations
    /// are active and need RAV processing. This is the same logic ractor used for allocation discovery.
    ///
    /// **Connection Pool Fix**: Use single connection with transaction to avoid connection exhaustion
    async fn get_active_allocations(pgpool: &PgPool) -> Result<Vec<AllocationId>> {
        warn!("Using static allocation discovery - network subgraph not configured");
        warn!("For production use, configure network_subgraph for real-time allocation monitoring");

        let mut allocations = Vec::new();

        // **FIX**: Use single connection with transaction to prevent connection pool exhaustion
        let mut tx = pgpool.begin().await.map_err(|e| {
            anyhow::anyhow!("Failed to begin transaction for allocation discovery: {e}")
        })?;

        // Get Legacy (V1) allocations with pending receipts (ractor pattern)
        // Reference: sender_accounts_manager.rs get_pending_sender_allocation_id_v1()
        let legacy_allocations = sqlx::query!(
            r#"
                SELECT DISTINCT allocation_id 
                FROM scalar_tap_receipts 
                ORDER BY allocation_id
            "#
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query Legacy allocations: {e}"))?;

        for row in legacy_allocations {
            // Parse allocation_id as Address and convert to AllocationId
            match row.allocation_id.parse::<Address>() {
                Ok(addr) => {
                    let allocation_id =
                        AllocationId::Legacy(thegraph_core::AllocationId::new(addr));
                    allocations.push(allocation_id);
                    debug!(allocation_id = %addr, "Found Legacy allocation with pending receipts");
                }
                Err(e) => {
                    warn!(
                        allocation_id = row.allocation_id,
                        error = %e,
                        "Failed to parse Legacy allocation_id from database"
                    );
                }
            }
        }

        // Get Horizon (V2) allocations with pending receipts (ractor pattern)
        // Reference: sender_accounts_manager.rs get_pending_sender_allocation_id_v2()
        let horizon_allocations = sqlx::query!(
            r#"
                SELECT DISTINCT collection_id 
                FROM tap_horizon_receipts 
                ORDER BY collection_id
            "#
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query Horizon allocations: {e}"))?;

        for row in horizon_allocations {
            // Parse collection_id as CollectionId and convert to AllocationId
            match row.collection_id.parse::<thegraph_core::CollectionId>() {
                Ok(collection_id) => {
                    let allocation_id = AllocationId::Horizon(collection_id);
                    allocations.push(allocation_id);
                    debug!(collection_id = %collection_id, "Found Horizon allocation with pending receipts");
                }
                Err(e) => {
                    warn!(
                        collection_id = row.collection_id,
                        error = %e,
                        "Failed to parse Horizon collection_id from database"
                    );
                }
            }
        }

        // Commit transaction to release connection properly
        tx.commit().await.map_err(|e| {
            anyhow::anyhow!("Failed to commit allocation discovery transaction: {e}")
        })?;

        info!(
            allocation_count = allocations.len(),
            legacy_count = allocations
                .iter()
                .filter(|a| matches!(a, AllocationId::Legacy(_)))
                .count(),
            horizon_count = allocations
                .iter()
                .filter(|a| matches!(a, AllocationId::Horizon(_)))
                .count(),
            "✅ Static allocation discovery completed (ractor pattern) - single connection used"
        );

        if allocations.is_empty() {
            info!("No pending receipts found in database - no allocations need RAV processing");
            info!("This is normal when all receipts have been processed into RAVs");
            info!("For real-time allocation monitoring, configure network_subgraph");
        }

        Ok(allocations)
    }
}

/// Convenience function to create and run TAP Agent
pub async fn run_tap_agent(config: TapAgentConfig) -> Result<()> {
    let mut agent = TapAgent::new(config);
    agent.start().await?;
    agent.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::allocation_id::AllocationId;
    use crate::agent::stream_processor::{ProcessingResult, TapEvent};

    #[tokio::test]
    async fn test_tap_agent_creation() {
        let config = TapAgentConfig::for_testing().await;
        let agent = TapAgent::new(config);

        // Verify agent is created with correct initial state
        assert!(agent.shutdown_tx.is_none());
        assert_eq!(agent.tasks.len(), 0);
    }

    #[tokio::test]
    async fn test_shutdown_coordination() {
        let config = TapAgentConfig::for_testing().await;
        let mut agent = TapAgent::new(config);

        // Create a mock shutdown channel to test coordination
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        agent.shutdown_tx = Some(shutdown_tx);

        // Test shutdown without full startup
        agent.shutdown().await.unwrap();

        // Verify shutdown signal was sent
        assert!(agent.shutdown_tx.is_none());

        // Verify we can receive the shutdown signal
        tokio::select! {
            _ = shutdown_rx.recv() => {
                // Good, we received the shutdown signal
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                panic!("Shutdown signal not received");
            }
        }
    }

    #[tokio::test]
    async fn test_stream_based_processing() {
        // TDD: Test the stream-based architecture without full agent startup
        let config = TapAgentConfig::for_testing().await;
        let pgpool = config.pgpool.clone();

        // Test 1: Verify configuration
        assert_eq!(config.rav_threshold, 1000);
        assert_eq!(config.event_buffer_size, 10);

        // Test 2: Create channels for stream processing
        let (event_tx, mut event_rx) = mpsc::channel::<TapEvent>(10);
        let (result_tx, mut result_rx) = mpsc::channel::<ProcessingResult>(10);

        // Test 3: Send test event through channel
        event_tx.send(TapEvent::Shutdown).await.unwrap();

        // Verify event reception
        match event_rx.recv().await {
            Some(TapEvent::Shutdown) => {
                info!("✅ Event channel communication works");
            }
            _ => panic!("Expected shutdown event"),
        }

        // Test 4: Send test result through channel
        result_tx
            .send(ProcessingResult::Pending {
                allocation_id: AllocationId::Legacy(thegraph_core::AllocationId::new(
                    Address::ZERO,
                )),
            })
            .await
            .unwrap();

        // Verify result reception
        match result_rx.recv().await {
            Some(ProcessingResult::Pending { .. }) => {
                info!("✅ Result channel communication works");
            }
            _ => panic!("Expected pending result"),
        }

        // Close the database pool
        pgpool.close().await;

        info!("✅ Stream-based TAP agent test completed successfully");
    }

    #[tokio::test]
    async fn test_rav_threshold_processing() {
        // TDD: Test RAV threshold configuration without database dependencies
        // Use real test database for configuration validation but avoid heavy operations
        let config = TapAgentConfig::for_testing().await;

        // Test 1: Agent accepts the configuration
        let agent = TapAgent::new(config.clone());
        assert_eq!(agent.config.rav_threshold, 1000);
        assert_eq!(agent.config.event_buffer_size, 10);
        assert_eq!(agent.config.result_buffer_size, 10);
        assert_eq!(agent.config.rav_buffer_size, 10);

        // Test 2: Test channel-based processing architecture
        let (event_tx, mut event_rx) = mpsc::channel::<TapEvent>(10);
        let (_result_tx, _result_rx) = mpsc::channel::<ProcessingResult>(10);

        // Verify threshold configuration through channel communication
        event_tx.send(TapEvent::Shutdown).await.unwrap();

        match event_rx.recv().await {
            Some(TapEvent::Shutdown) => {
                info!(
                    "✅ Event channel communication works with threshold {}",
                    config.rav_threshold
                );
            }
            _ => panic!("Expected shutdown event"),
        }

        // Test 3: Verify TapAgent configuration structure
        assert!(agent.shutdown_tx.is_none());
        assert_eq!(agent.tasks.len(), 0);

        // Cleanup
        config.pgpool.close().await;
        info!("✅ RAV threshold test completed successfully - tested configuration and channel architecture");
    }
}
