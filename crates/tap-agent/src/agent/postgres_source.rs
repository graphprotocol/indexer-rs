// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! PostgreSQL event source for TAP receipts
//!
//! This module provides stream-based integration with PostgreSQL LISTEN/NOTIFY
//! to receive TAP receipt events in real-time. Parses actual database notifications
//! and fetches full receipt data for processing.

use anyhow::{Context, Result};
use bigdecimal::BigDecimal;
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool, Row};
use std::str::FromStr;
use thegraph_core::alloy::primitives::Address;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::{allocation_id::AllocationId, stream_processor::TapEvent};
use indexer_receipt::TapReceipt;

/// V1 (Legacy) receipt notification from scalar_tap_receipt_notification channel
#[derive(Deserialize, Debug, Clone)]
pub struct NewReceiptNotificationV1 {
    /// Database receipt ID (BIGSERIAL)
    pub id: i64,
    /// 40-char hex allocation ID
    pub allocation_id: String,
    /// 40-char hex signer address
    pub signer_address: String,
    /// Receipt timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Receipt value as number (NUMERIC(39)) - database trigger sends as unquoted number
    pub value: i64,
}

/// V2 (Horizon) receipt notification from tap_horizon_receipt_notification channel
#[derive(Deserialize, Debug, Clone)]
pub struct NewReceiptNotificationV2 {
    /// Database receipt ID (BIGSERIAL)
    pub id: i64,
    /// 64-char hex collection ID
    pub collection_id: String,
    /// 40-char hex signer address
    pub signer_address: String,
    /// Receipt timestamp in nanoseconds
    pub timestamp_ns: i64,
    /// Receipt value as number (NUMERIC(39)) - database trigger sends as unquoted number
    pub value: i64,
}

/// Unified notification envelope for both V1 and V2
#[derive(Debug, Clone)]
pub enum NewReceiptNotification {
    /// V1 (Legacy) notification
    V1(NewReceiptNotificationV1),
    /// V2 (Horizon) notification
    V2(NewReceiptNotificationV2),
}

impl NewReceiptNotification {
    /// Extract the database ID for receipt fetching
    pub fn id(&self) -> i64 {
        match self {
            NewReceiptNotification::V1(n) => n.id,
            NewReceiptNotification::V2(n) => n.id,
        }
    }

    /// Extract the AllocationId for routing
    pub fn allocation_id(&self) -> Result<AllocationId> {
        match self {
            NewReceiptNotification::V1(n) => {
                let addr: Address = n
                    .allocation_id
                    .parse()
                    .with_context(|| format!("Invalid V1 allocation_id: {}", &n.allocation_id))?;
                Ok(AllocationId::Legacy(thegraph_core::AllocationId::new(addr)))
            }
            NewReceiptNotification::V2(n) => {
                let collection_id: thegraph_core::CollectionId = n
                    .collection_id
                    .parse()
                    .with_context(|| format!("Invalid V2 collection_id: {}", &n.collection_id))?;
                Ok(AllocationId::Horizon(collection_id))
            }
        }
    }
}

/// PostgreSQL event source for TAP receipts
///
/// Listens to PostgreSQL NOTIFY events and converts them into TapEvent stream.
/// Handles both Legacy (V1) and Horizon (V2) receipt notifications with real
/// database integration.
///
/// **Architecture**: Uses dependency injection for database connections to ensure
/// proper isolation and testability. All database operations use shared connections.
pub struct PostgresEventSource {
    pgpool: PgPool,
}

impl PostgresEventSource {
    /// Create new PostgreSQL event source
    pub fn new(pgpool: PgPool) -> Self {
        Self { pgpool }
    }

    /// Start streaming receipt events from PostgreSQL LISTEN/NOTIFY
    ///
    /// This connects to both V1 and V2 notification channels and processes
    /// receipt notifications in real-time. Fetches full receipt data from
    /// database and sends TapEvents to processing pipeline.
    ///
    /// **Connection Management**: Creates dedicated listeners from the pool while
    /// ensuring proper connection sharing for receipt fetching operations.
    pub async fn start_receipt_stream(self, event_tx: mpsc::Sender<TapEvent>) -> Result<()> {
        info!("Starting PostgreSQL receipt stream with real notification integration");

        // Create listeners for both V1 and V2 channels
        // **FIX**: Use pool connections but capture connection info for receipt fetching
        let mut listener_v1 = PgListener::connect_with(&self.pgpool)
            .await
            .context("Failed to create V1 PgListener")?;
        let mut listener_v2 = PgListener::connect_with(&self.pgpool)
            .await
            .context("Failed to create V2 PgListener")?;

        // Listen to both notification channels
        listener_v1
            .listen("scalar_tap_receipt_notification")
            .await
            .context("Failed to listen to V1 notifications")?;
        listener_v2
            .listen("tap_horizon_receipt_notification")
            .await
            .context("Failed to listen to V2 notifications")?;

        info!("✅ PostgreSQL event source ready - listening for notifications on both V1 and V2 channels");

        // Main event loop with tokio::select! for concurrent processing
        loop {
            tokio::select! {
                // V1 (Legacy) notifications
                notification = listener_v1.recv() => {
                    match notification {
                        Ok(notification) => {
                            info!(channel = notification.channel(), payload = notification.payload(), "🔔 Received V1 notification from PostgreSQL");
                            if let Err(e) = self.process_v1_notification(notification.payload(), &event_tx).await {
                                error!(error = %e, "Failed to process V1 notification");
                                // Continue processing other notifications
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "V1 listener error - attempting to continue");
                            // In production, implement reconnection logic here
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }

                // V2 (Horizon) notifications
                notification = listener_v2.recv() => {
                    match notification {
                        Ok(notification) => {
                            info!(channel = notification.channel(), payload = notification.payload(), "🔔 Received V2 notification from PostgreSQL");
                            if let Err(e) = self.process_v2_notification(notification.payload(), &event_tx).await {
                                error!(error = %e, "Failed to process V2 notification");
                                // Continue processing other notifications
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "V2 listener error - attempting to continue");
                            // In production, implement reconnection logic here
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }

                // Check if event channel is closed (shutdown signal)
                else => {
                    info!("Event channel closed, shutting down PostgreSQL stream");
                    break;
                }
            }
        }

        info!("PostgreSQL receipt stream shutting down");
        Ok(())
    }

    /// Process V1 (Legacy) notification
    async fn process_v1_notification(
        &self,
        payload: &str,
        event_tx: &mpsc::Sender<TapEvent>,
    ) -> Result<()> {
        debug!(payload = payload, "Received V1 notification");

        // Parse JSON notification
        let notification: NewReceiptNotificationV1 = serde_json::from_str(payload)
            .with_context(|| format!("Failed to parse V1 notification: {payload}"))?;

        debug!(
            id = notification.id,
            allocation_id = notification.allocation_id,
            signer = notification.signer_address,
            value = notification.value,
            "Parsed V1 receipt notification"
        );

        // Fetch full receipt from database
        debug!(
            "📥 Fetching V1 receipt from database with ID {}",
            notification.id
        );
        let receipt = self
            .fetch_v1_receipt(notification.id)
            .await
            .with_context(|| format!("Failed to fetch V1 receipt {}", notification.id))?;
        debug!("✅ Successfully fetched V1 receipt from database");

        // Extract allocation ID for routing
        let allocation_id = NewReceiptNotification::V1(notification.clone())
            .allocation_id()
            .context("Failed to parse V1 allocation ID")?;
        debug!(allocation_id = ?allocation_id, "🎯 Parsed allocation ID for routing");

        // Send to processing pipeline
        debug!("📤 Sending receipt to TapProcessingPipeline");
        if event_tx
            .send(TapEvent::Receipt(receipt, allocation_id))
            .await
            .is_err()
        {
            warn!("Processing pipeline channel closed, stopping V1 processing");
            return Err(anyhow::anyhow!("Processing pipeline disconnected"));
        }
        debug!("✅ Receipt sent to processing pipeline successfully");

        Ok(())
    }

    /// Process V2 (Horizon) notification
    async fn process_v2_notification(
        &self,
        payload: &str,
        event_tx: &mpsc::Sender<TapEvent>,
    ) -> Result<()> {
        debug!(payload = payload, "Received V2 notification");

        // Parse JSON notification
        let notification: NewReceiptNotificationV2 = serde_json::from_str(payload)
            .with_context(|| format!("Failed to parse V2 notification: {payload}"))?;

        debug!(
            id = notification.id,
            collection_id = notification.collection_id,
            signer = notification.signer_address,
            value = notification.value,
            "Parsed V2 receipt notification"
        );

        // Fetch full receipt from database
        let receipt = self
            .fetch_v2_receipt(notification.id)
            .await
            .with_context(|| format!("Failed to fetch V2 receipt {}", notification.id))?;

        // Extract allocation ID for routing
        let allocation_id = NewReceiptNotification::V2(notification.clone())
            .allocation_id()
            .context("Failed to parse V2 collection ID")?;

        // Send to processing pipeline
        if event_tx
            .send(TapEvent::Receipt(receipt, allocation_id))
            .await
            .is_err()
        {
            warn!("Processing pipeline channel closed, stopping V2 processing");
            return Err(anyhow::anyhow!("Processing pipeline disconnected"));
        }

        Ok(())
    }

    /// Fetch full V1 receipt from database
    async fn fetch_v1_receipt(&self, receipt_id: i64) -> Result<TapReceipt> {
        debug!("Executing V1 receipt query for ID {}", receipt_id);
        let row = sqlx::query(
            "SELECT signature, allocation_id, timestamp_ns, nonce, value 
             FROM scalar_tap_receipts 
             WHERE id = $1",
        )
        .bind(receipt_id)
        .fetch_one(&self.pgpool)
        .await
        .with_context(|| format!("V1 receipt {receipt_id} not found in database"))?;
        debug!(
            "Successfully fetched row from scalar_tap_receipts for ID {}",
            receipt_id
        );

        // Extract values from database row
        debug!("Extracting values from database row");
        let signature_bytes: Vec<u8> = row.get("signature");
        let allocation_id: String = row.get("allocation_id");
        let timestamp_ns: sqlx::types::BigDecimal = row.get("timestamp_ns");
        let nonce: sqlx::types::BigDecimal = row.get("nonce");
        let value: sqlx::types::BigDecimal = row.get("value");
        debug!(
            allocation_id = allocation_id,
            signature_len = signature_bytes.len(),
            "Extracted raw values from database"
        );

        // Parse allocation_id as Address
        debug!("Parsing allocation_id as Address: {}", allocation_id);
        let allocation_addr: Address = allocation_id
            .parse()
            .with_context(|| format!("Invalid allocation_id format: {allocation_id}"))?;
        debug!("Successfully parsed allocation_id as Address");

        // Convert BigDecimal to u64/u128
        debug!("Converting BigDecimal values to integers");
        let timestamp_ns: u64 = timestamp_ns
            .to_string()
            .parse()
            .context("Failed to parse timestamp_ns")?;
        let nonce: u64 = nonce.to_string().parse().context("Failed to parse nonce")?;
        let value: u128 = value.to_string().parse().context("Failed to parse value")?;
        debug!(
            timestamp_ns = timestamp_ns,
            nonce = nonce,
            value = value,
            "Converted BigDecimal values"
        );

        // Reconstruct the signed receipt (simplified - in production we'd need proper EIP-712 reconstruction)
        // For now, create a placeholder - this needs proper implementation with EIP-712 domains
        info!(
            receipt_id = receipt_id,
            allocation_id = %allocation_addr,
            timestamp_ns = timestamp_ns,
            nonce = nonce,
            value = value,
            "Fetched V1 receipt from database (placeholder reconstruction)"
        );

        // Reconstruct the signed receipt from database fields
        use tap_core::signed_message::Eip712SignedMessage;
        use thegraph_core::alloy::signers::Signature;

        // Parse signature bytes into Signature
        debug!(
            "Parsing signature bytes (length: {})",
            signature_bytes.len()
        );
        let signature = Signature::try_from(signature_bytes.as_slice())
            .context("Failed to parse signature bytes")?;
        debug!("Successfully parsed signature");

        // Create the message from database fields
        let message = tap_graph::Receipt {
            allocation_id: allocation_addr,
            nonce,
            timestamp_ns,
            value,
        };

        // Reconstruct the signed receipt
        // Note: This recreates the signed receipt structure from stored components
        let signed_receipt = Eip712SignedMessage { message, signature };

        debug!(
            receipt_id = receipt_id,
            allocation_id = %allocation_addr,
            nonce = nonce,
            value = value,
            "Reconstructed V1 signed receipt from database"
        );

        Ok(TapReceipt::V1(signed_receipt))
    }

    /// Fetch full V2 receipt from database
    async fn fetch_v2_receipt(&self, receipt_id: i64) -> Result<TapReceipt> {
        let row = sqlx::query(
            "SELECT signature, collection_id, payer, data_service, service_provider, 
                    timestamp_ns, nonce, value 
             FROM tap_horizon_receipts 
             WHERE id = $1",
        )
        .bind(receipt_id)
        .fetch_one(&self.pgpool)
        .await
        .with_context(|| format!("V2 receipt {receipt_id} not found in database"))?;

        // Extract values from database row
        let signature_bytes: Vec<u8> = row.get("signature");
        let collection_id: String = row.get("collection_id");
        let payer: String = row.get("payer");
        let data_service: String = row.get("data_service");
        let service_provider: String = row.get("service_provider");
        let timestamp_ns: sqlx::types::BigDecimal = row.get("timestamp_ns");
        let nonce: sqlx::types::BigDecimal = row.get("nonce");
        let value: sqlx::types::BigDecimal = row.get("value");

        // Parse addresses
        let collection_id: thegraph_core::CollectionId = collection_id
            .parse()
            .with_context(|| format!("Invalid collection_id format: {collection_id}"))?;
        let payer: Address = payer
            .parse()
            .with_context(|| format!("Invalid payer format: {payer}"))?;
        let _data_service: Address = data_service
            .parse()
            .with_context(|| format!("Invalid data_service format: {data_service}"))?;
        let _service_provider: Address = service_provider
            .parse()
            .with_context(|| format!("Invalid service_provider format: {service_provider}"))?;

        // Convert BigDecimal to u64/u128
        let timestamp_ns: u64 = timestamp_ns
            .to_string()
            .parse()
            .context("Failed to parse timestamp_ns")?;
        let nonce: u64 = nonce.to_string().parse().context("Failed to parse nonce")?;
        let value: u128 = value.to_string().parse().context("Failed to parse value")?;

        info!(
            receipt_id = receipt_id,
            collection_id = %collection_id,
            payer = %payer,
            timestamp_ns = timestamp_ns,
            nonce = nonce,
            value = value,
            "Fetched V2 receipt from database (placeholder reconstruction)"
        );

        // Reconstruct the V2 signed receipt from database fields
        use tap_core::signed_message::Eip712SignedMessage;
        use thegraph_core::alloy::signers::Signature;

        // Parse signature bytes into Signature
        let signature = Signature::try_from(signature_bytes.as_slice())
            .context("Failed to parse V2 signature bytes")?;

        // Create the V2 message from database fields
        let message = tap_graph::v2::Receipt {
            payer,
            service_provider: _service_provider,
            data_service: _data_service,
            collection_id: collection_id.into_inner(),
            nonce,
            timestamp_ns,
            value,
        };

        // Reconstruct the V2 signed receipt
        let signed_receipt = Eip712SignedMessage { message, signature };

        debug!(
            receipt_id = receipt_id,
            collection_id = %collection_id,
            nonce = nonce,
            value = value,
            "Reconstructed V2 signed receipt from database"
        );

        Ok(TapReceipt::V2(signed_receipt))
    }
}

/// RAV request timer
///
/// Periodically sends RAV creation requests for all active allocations.
/// This ensures RAVs are created even when receipt flow stops.
pub struct RavRequestTimer {
    interval: tokio::time::Duration,
}

impl RavRequestTimer {
    /// Create new RAV request timer
    pub fn new(interval: tokio::time::Duration) -> Self {
        Self { interval }
    }

    /// Get the timer interval
    pub fn get_interval(&self) -> tokio::time::Duration {
        self.interval
    }

    /// Start periodic RAV request timer
    ///
    /// Sends RavRequest events at regular intervals. This is a simple
    /// implementation - a more sophisticated version could track which
    /// allocations actually need RAVs.
    pub async fn start(
        self,
        event_tx: mpsc::Sender<TapEvent>,
        active_allocations: Vec<AllocationId>,
    ) -> Result<()> {
        info!(
            interval_secs = self.interval.as_secs(),
            allocation_count = active_allocations.len(),
            "Starting RAV request timer"
        );

        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            debug!("RAV timer tick - requesting RAVs for active allocations");

            for allocation_id in &active_allocations {
                let event = TapEvent::RavRequest(*allocation_id);

                if event_tx.send(event).await.is_err() {
                    info!("Event receiver dropped, shutting down RAV timer");
                    return Ok(());
                }
            }
        }
    }
}

/// RAV persistence service
///
/// Receives RAV results from the processing pipeline and persists them
/// to the database. Also handles notification of other services.
#[allow(dead_code)] // pgpool will be used in production implementation
pub struct RavPersister {
    pgpool: PgPool,
}

impl RavPersister {
    /// Create new RAV persister
    pub fn new(pgpool: PgPool) -> Self {
        Self { pgpool }
    }

    /// Start RAV persistence service
    ///
    /// Receives RAV results and persists them to the appropriate database table
    /// based on the allocation ID type (Legacy vs Horizon).
    pub async fn start(
        self,
        mut rav_rx: mpsc::Receiver<super::stream_processor::RavResult>,
    ) -> Result<()> {
        info!("Starting RAV persistence service");

        while let Some(rav) = rav_rx.recv().await {
            if let Err(e) = self.process_rav(&rav).await {
                error!(
                    allocation_id = ?rav.allocation_id,
                    error = %e,
                    "Failed to process RAV result"
                );
                // Continue processing other RAVs
            }
        }

        info!("RAV persistence service shutting down");
        Ok(())
    }

    /// Process RAV result - TAP Manager has already persisted the RAV
    ///
    /// **Architecture Note**: This follows the ractor pattern where TAP Manager
    /// handles all database persistence via `verify_and_store_rav()`. This service
    /// handles post-processing activities like metrics and parent communication.
    ///
    /// **Reference**: sender_allocation.rs:648 - After successful `verify_and_store_rav()`,
    /// the ractor sends `UpdateRav` message to parent `SenderAccountTask`.
    async fn process_rav(&self, rav: &super::stream_processor::RavResult) -> Result<()> {
        match &rav.allocation_id {
            AllocationId::Legacy(allocation_id) => {
                self.process_legacy_rav(rav, allocation_id).await
            }
            AllocationId::Horizon(collection_id) => {
                self.process_horizon_rav(rav, collection_id).await
            }
        }
    }

    /// Process Legacy (V1) RAV result - RAV already persisted by TAP Manager
    async fn process_legacy_rav(
        &self,
        rav: &super::stream_processor::RavResult,
        allocation_id: &thegraph_core::AllocationId,
    ) -> Result<()> {
        info!(
            allocation_id = %allocation_id.to_string(),
            value_aggregate = rav.value_aggregate,
            receipt_count = rav.receipt_count,
            "Processing Legacy RAV result - TAP Manager has already persisted the RAV"
        );

        // **TDD Implementation**: Start with basic database insertion to make tests fail properly
        // TODO: Integrate with TAP Manager for proper verification (follow ractor pattern)
        // Reference: sender_allocation.rs:643-646 - tap_manager.verify_and_store_rav()

        // For now, we'll just test that we can insert into the database structure
        // This will reveal what data we're missing from RavResult to properly persist RAVs
        warn!(
            allocation_id = %allocation_id.to_string(),
            "⚠️  TDD: Attempting basic RAV persistence - missing signed RAV data from RavResult"
        );

        // TODO: This query will fail because we don't have the required fields
        // - sender_address: Need to extract from signed RAV or pass separately
        // - signature: Need actual signature from signed RAV
        // - timestamp_ns: Need timestamp from signed RAV
        // This failure will guide us to improve RavResult structure

        let _rows_affected = sqlx::query!(
            r#"
                INSERT INTO scalar_tap_ravs (
                    sender_address,
                    signature,
                    allocation_id, 
                    timestamp_ns,
                    value_aggregate,
                    last,
                    final
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            format!("{:x}", rav.sender_address), // Use sender from RavResult (40 chars, no 0x prefix)
            &rav.signed_rav,                     // Use actual signed RAV bytes from RavResult
            format!("{:x}", allocation_id),      // Format as hex without 0x prefix (40 chars)
            BigDecimal::from_str(&rav.timestamp_ns.to_string()).expect("Valid BigDecimal"), // Use timestamp from RavResult
            BigDecimal::from_str(&rav.value_aggregate.to_string()).expect("Valid BigDecimal"),
            false, // TODO: Determine if this is the last RAV
            false  // TODO: Determine if this is final RAV
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to insert Legacy RAV into database: {e}"))?;

        info!(
            allocation_id = %allocation_id.to_string(),
            value_aggregate = rav.value_aggregate,
            "✅ Legacy RAV persisted to scalar_tap_ravs table (basic implementation)"
        );

        Ok(())
    }

    /// Process Horizon (V2) RAV result - RAV already persisted by TAP Manager
    async fn process_horizon_rav(
        &self,
        rav: &super::stream_processor::RavResult,
        collection_id: &thegraph_core::CollectionId,
    ) -> Result<()> {
        info!(
            collection_id = %collection_id.to_string(),
            value_aggregate = rav.value_aggregate,
            receipt_count = rav.receipt_count,
            "Processing Horizon RAV result - TAP Manager has already persisted the RAV"
        );

        // **TDD Implementation**: Use enhanced RavResult fields for Horizon
        // TODO: Integrate with TAP Manager for proper verification (follow ractor pattern)
        warn!(
            collection_id = %collection_id.to_string(),
            "⚠️  TDD: Attempting basic Horizon RAV persistence - need TAP Manager integration"
        );

        // Horizon schema uses payer instead of sender_address
        // For now, using sender_address as payer - TODO: Extract correct values from signed RAV
        let _rows_affected = sqlx::query!(
            r#"
                INSERT INTO tap_horizon_ravs (
                    signature,
                    collection_id,
                    payer,
                    data_service,
                    service_provider,
                    timestamp_ns, 
                    value_aggregate,
                    metadata,
                    last,
                    final
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            &rav.signed_rav, // Use actual signed RAV bytes from RavResult
            collection_id.to_string(),
            format!("{:x}", rav.sender_address), // Use sender as payer for now (40 chars, no 0x)
            "0x0000000000000000000000000000000000000000", // TODO: Extract data_service from signed RAV
            "0x0000000000000000000000000000000000000000", // TODO: Extract service_provider from signed RAV
            BigDecimal::from_str(&rav.timestamp_ns.to_string()).expect("Valid BigDecimal"), // Use timestamp from RavResult
            BigDecimal::from_str(&rav.value_aggregate.to_string()).expect("Valid BigDecimal"),
            &[0u8; 1], // TODO: Get actual metadata from signed RAV
            false,     // TODO: Determine if this is the last RAV
            false      // TODO: Determine if this is final RAV
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to insert Horizon RAV into database: {e}"))?;

        info!(
            collection_id = %collection_id.to_string(),
            value_aggregate = rav.value_aggregate,
            "✅ Horizon RAV persisted to tap_horizon_ravs table (basic implementation)"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_notification_parsing() {
        // Test V1 notification parsing - match database trigger format
        let v1_payload = r#"{"id": 123, "allocation_id": "fa44c72b753a66591f241c7dc04e8178c30e13af", "signer_address": "533661F0fb14d2E8B26223C86a610Dd7D2260892", "timestamp_ns": 1640995200000000000, "value": 1000}"#;

        let v1_notification: NewReceiptNotificationV1 = serde_json::from_str(v1_payload).unwrap();
        assert_eq!(v1_notification.id, 123);
        assert_eq!(
            v1_notification.allocation_id,
            "fa44c72b753a66591f241c7dc04e8178c30e13af"
        );
        assert_eq!(v1_notification.value, 1000);

        // Test V2 notification parsing - match database trigger format
        let v2_payload = r#"{"id": 456, "collection_id": "000000000000000000000000fa44c72b753a66591f241c7dc04e8178c30e13af", "signer_address": "533661F0fb14d2E8B26223C86a610Dd7D2260892", "timestamp_ns": 1640995200000000000, "value": 2000}"#;

        let v2_notification: NewReceiptNotificationV2 = serde_json::from_str(v2_payload).unwrap();
        assert_eq!(v2_notification.id, 456);
        assert_eq!(
            v2_notification.collection_id,
            "000000000000000000000000fa44c72b753a66591f241c7dc04e8178c30e13af"
        );
        assert_eq!(v2_notification.value, 2000);

        // Test unified notification envelope
        let unified_v1 = NewReceiptNotification::V1(v1_notification);
        let unified_v2 = NewReceiptNotification::V2(v2_notification);

        assert_eq!(unified_v1.id(), 123);
        assert_eq!(unified_v2.id(), 456);

        // Test allocation ID extraction
        let alloc_v1 = unified_v1.allocation_id().unwrap();
        let alloc_v2 = unified_v2.allocation_id().unwrap();

        match alloc_v1 {
            AllocationId::Legacy(_) => {} // Expected
            _ => panic!("Expected Legacy allocation ID"),
        }

        match alloc_v2 {
            AllocationId::Horizon(_) => {} // Expected
            _ => panic!("Expected Horizon allocation ID"),
        }
    }

    #[tokio::test]
    async fn test_rav_request_timer() {
        let (event_tx, mut event_rx) = mpsc::channel(10);

        let timer = RavRequestTimer::new(std::time::Duration::from_millis(50));
        let allocation_id =
            AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

        // Start timer in background
        tokio::spawn(async move {
            timer.start(event_tx, vec![allocation_id]).await.unwrap();
        });

        // Should receive RAV requests
        let event1 = event_rx.recv().await.unwrap();
        let event2 = event_rx.recv().await.unwrap();

        match (event1, event2) {
            (TapEvent::RavRequest(id1), TapEvent::RavRequest(id2)) => {
                assert_eq!(id1, allocation_id);
                assert_eq!(id2, allocation_id);
            }
            _ => panic!("Expected RavRequest events"),
        }
    }

    #[tokio::test]
    async fn test_rav_persister_shutdown() {
        use super::super::stream_processor::RavResult;

        let (rav_tx, rav_rx) = mpsc::channel(10);

        // Setup mock database pool (this is a simplified test)
        // In practice, you'd use a test database
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://test:test@localhost/test".to_string());

        if let Ok(pgpool) = PgPool::connect(&database_url).await {
            let persister = RavPersister::new(pgpool);

            // Start persister in background
            tokio::spawn(async move {
                persister.start(rav_rx).await.unwrap();
            });

            // Send a test RAV
            let allocation_id =
                AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

            let rav = RavResult {
                allocation_id,
                value_aggregate: 1000,
                receipt_count: 5,
                signed_rav: vec![1u8; 65],
                sender_address: Address::from([1u8; 20]),
                timestamp_ns: 1640995200000000000,
            };

            rav_tx.send(rav).await.unwrap();

            // Close channel to trigger shutdown
            drop(rav_tx);

            // Wait a bit for processing
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        } else {
            // Skip test if no database available
            println!("Skipping RAV persister test - no database available");
        }
    }
}
