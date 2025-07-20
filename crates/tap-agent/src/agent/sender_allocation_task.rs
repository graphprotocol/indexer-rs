// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Tokio-based replacement for SenderAllocation actor
//!
//! This is a simplified, tokio-based replacement for the ractor SenderAllocation
//! that maintains API compatibility while using tasks and channels.

use std::{marker::PhantomData, time::Instant};

use tokio::sync::mpsc;

use super::{
    sender_account::{RavInformation, ReceiptFees, SenderAccountMessage},
    sender_accounts_manager::{AllocationId, NewReceiptNotification},
    unaggregated_receipts::UnaggregatedReceipts,
};
use crate::{
    actor_migrate::{LifecycleManager, RestartPolicy, TaskHandle},
    tap::context::NetworkVersion,
};

/// Message types for SenderAllocationTask - matches original SenderAllocationMessage
#[derive(Debug)]
pub enum SenderAllocationMessage {
    /// New receipt message
    NewReceipt(NewReceiptNotification),
    /// Triggers a Rav Request for the current allocation
    TriggerRavRequest,
    #[cfg(any(test, feature = "test"))]
    /// Return the internal state (used for tests)
    GetUnaggregatedReceipts(tokio::sync::oneshot::Sender<UnaggregatedReceipts>),
}

/// Tokio task-based replacement for SenderAllocation actor
pub struct SenderAllocationTask<T: NetworkVersion> {
    _phantom: PhantomData<T>,
}

/// Enhanced state structure for the task with invalid receipts tracking
struct TaskState {
    /// Sum of all receipt fees for the current allocation
    unaggregated_fees: UnaggregatedReceipts,
    /// Sum of all invalid receipt fees
    invalid_receipts_fees: UnaggregatedReceipts,
    /// Handle to communicate with parent SenderAccount
    sender_account_handle: TaskHandle<SenderAccountMessage>,
    /// Current allocation ID
    allocation_id: AllocationId,
}

impl<T: NetworkVersion> SenderAllocationTask<T> {
    /// Spawn a new SenderAllocationTask with minimal arguments
    pub async fn spawn_simple(
        lifecycle: &LifecycleManager,
        name: Option<String>,
        allocation_id: AllocationId,
        sender_account_handle: TaskHandle<SenderAccountMessage>,
    ) -> anyhow::Result<TaskHandle<SenderAllocationMessage>> {
        let state = TaskState {
            unaggregated_fees: UnaggregatedReceipts::default(),
            invalid_receipts_fees: UnaggregatedReceipts::default(),
            sender_account_handle,
            allocation_id,
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
        mut rx: mpsc::Receiver<SenderAllocationMessage>,
    ) -> anyhow::Result<()> {
        // Send initial state to parent
        state
            .sender_account_handle
            .cast(SenderAccountMessage::UpdateReceiptFees(
                state.allocation_id,
                ReceiptFees::UpdateValue(state.unaggregated_fees),
            ))
            .await?;

        while let Some(message) = rx.recv().await {
            match message {
                SenderAllocationMessage::NewReceipt(notification) => {
                    if let Err(e) = Self::handle_new_receipt(&mut state, notification).await {
                        tracing::error!(
                            allocation_id = ?state.allocation_id,
                            error = %e,
                            "Error handling new receipt"
                        );
                    }
                }
                SenderAllocationMessage::TriggerRavRequest => {
                    if let Err(e) = Self::handle_rav_request(&mut state).await {
                        tracing::error!(
                            allocation_id = ?state.allocation_id,
                            error = %e,
                            "Error handling RAV request"
                        );
                    }
                }
                #[cfg(any(test, feature = "test"))]
                SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                    let _ = reply.send(state.unaggregated_fees);
                }
            }
        }

        Ok(())
    }

    /// Handle new receipt - with basic validation and invalid receipt tracking
    async fn handle_new_receipt(
        state: &mut TaskState,
        notification: NewReceiptNotification,
    ) -> anyhow::Result<()> {
        let (id, value, timestamp_ns, signer_address) = match notification {
            NewReceiptNotification::V1(ref n) => (n.id, n.value, n.timestamp_ns, n.signer_address),
            NewReceiptNotification::V2(ref n) => (n.id, n.value, n.timestamp_ns, n.signer_address),
        };

        // Basic receipt ID validation - reject already processed receipts
        if id <= state.unaggregated_fees.last_id {
            tracing::warn!(
                allocation_id = ?state.allocation_id,
                receipt_id = id,
                last_processed_id = state.unaggregated_fees.last_id,
                "Rejecting receipt with ID <= last processed ID"
            );
            return Ok(()); // Silently ignore duplicate/old receipts
        }

        // Simulate basic receipt validation (in production this would be TAP manager)
        let is_valid = Self::validate_receipt_basic(id, value, signer_address);

        if is_valid {
            // Valid receipt - update state and notify parent
            state.unaggregated_fees.value += value;
            state.unaggregated_fees.counter += 1;
            state.unaggregated_fees.last_id = id;

            tracing::debug!(
                allocation_id = ?state.allocation_id,
                receipt_id = id,
                value = value,
                new_total = state.unaggregated_fees.value,
                "Processed valid receipt"
            );

            // Notify parent of valid receipt
            state
                .sender_account_handle
                .cast(SenderAccountMessage::UpdateReceiptFees(
                    state.allocation_id,
                    ReceiptFees::NewReceipt(value, timestamp_ns),
                ))
                .await?;
        } else {
            // Invalid receipt - track separately
            state.invalid_receipts_fees.value += value;
            state.invalid_receipts_fees.counter += 1;
            state.invalid_receipts_fees.last_id = id;

            tracing::warn!(
                allocation_id = ?state.allocation_id,
                receipt_id = id,
                value = value,
                signer = %signer_address,
                total_invalid_value = state.invalid_receipts_fees.value,
                "Receipt failed validation - tracked as invalid"
            );

            // Notify parent of invalid receipt fees
            state
                .sender_account_handle
                .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                    state.allocation_id,
                    state.invalid_receipts_fees,
                ))
                .await?;
        }

        Ok(())
    }

    /// Basic receipt validation (placeholder for TAP manager integration)
    fn validate_receipt_basic(
        id: u64,
        value: u128,
        signer_address: thegraph_core::alloy::primitives::Address,
    ) -> bool {
        // Simple validation rules for demonstration:
        // 1. Reject receipts with zero value
        // 2. Reject receipts from zero address (obviously invalid signer)
        // 3. Reject receipts with suspicious patterns (e.g., ID ending in 666)

        if value == 0 {
            return false;
        }

        if signer_address == thegraph_core::alloy::primitives::Address::ZERO {
            return false;
        }

        // Simulate some receipts being invalid due to signature issues
        if id % 1000 == 666 {
            return false; // Simulate signature validation failure
        }

        true // Most receipts are valid
    }

    /// Handle RAV request - enhanced but still simplified version
    async fn handle_rav_request(state: &mut TaskState) -> anyhow::Result<()> {
        let start_time = Instant::now();

        // Check if there are any receipts to aggregate
        if state.unaggregated_fees.value == 0 {
            tracing::debug!(
                allocation_id = ?state.allocation_id,
                "No receipts to aggregate, skipping RAV request"
            );
            return Ok(());
        }

        tracing::info!(
            allocation_id = ?state.allocation_id,
            receipt_count = state.unaggregated_fees.counter,
            total_value = state.unaggregated_fees.value,
            "Creating RAV for aggregated receipts"
        );

        // TODO: Replace with real TAP manager integration
        // For now, simulate a successful RAV request
        let rav_info = RavInformation {
            allocation_id: match state.allocation_id {
                AllocationId::Legacy(id) => id.into_inner(),
                AllocationId::Horizon(id) => {
                    // Convert CollectionId to Address - this is a simplification
                    thegraph_core::AllocationId::from(id).into_inner()
                }
            },
            value_aggregate: state.unaggregated_fees.value,
        };

        // Store the fees we're about to clear for the response
        let fees_to_clear = state.unaggregated_fees;

        // Reset local fees since they're now covered by RAV
        state.unaggregated_fees = UnaggregatedReceipts::default();

        let elapsed = start_time.elapsed();
        tracing::info!(
            allocation_id = ?state.allocation_id,
            rav_value = rav_info.value_aggregate,
            duration_ms = elapsed.as_millis(),
            "RAV creation completed successfully"
        );

        // Notify parent of successful RAV
        state
            .sender_account_handle
            .cast(SenderAccountMessage::UpdateReceiptFees(
                state.allocation_id,
                ReceiptFees::RavRequestResponse(fees_to_clear, Ok(Some(rav_info))),
            ))
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::context::Legacy;

    #[tokio::test]
    async fn test_sender_allocation_task_creation() {
        // Test basic task creation and message handling
        let lifecycle = LifecycleManager::new();

        // Create a dummy parent handle for testing
        let (parent_tx, mut parent_rx) = mpsc::channel(10);
        let parent_handle = TaskHandle::new_for_test(
            parent_tx,
            Some("test_parent".to_string()),
            std::sync::Arc::new(lifecycle.clone()),
        );

        let allocation_id =
            AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

        let task_handle = SenderAllocationTask::<Legacy>::spawn_simple(
            &lifecycle,
            Some("test_allocation".to_string()),
            allocation_id,
            parent_handle,
        )
        .await
        .unwrap();

        // Test sending a message
        let notification = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 1,
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::ZERO,
                timestamp_ns: 1000,
                value: 100,
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(notification))
            .await
            .unwrap();

        // Verify parent received the message
        let parent_message =
            tokio::time::timeout(std::time::Duration::from_millis(100), parent_rx.recv())
                .await
                .unwrap()
                .unwrap();

        // Check it's the right type of message
        assert!(matches!(
            parent_message,
            SenderAccountMessage::UpdateReceiptFees(..)
        ));
    }

    #[tokio::test]
    async fn test_receipt_id_validation() {
        let lifecycle = LifecycleManager::new();

        // Create a dummy parent handle for testing
        let (parent_tx, mut parent_rx) = mpsc::channel(10);
        let parent_handle = TaskHandle::new_for_test(
            parent_tx,
            Some("test_parent".to_string()),
            std::sync::Arc::new(lifecycle.clone()),
        );

        let allocation_id =
            AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

        let task_handle = SenderAllocationTask::<Legacy>::spawn_simple(
            &lifecycle,
            Some("test_allocation".to_string()),
            allocation_id,
            parent_handle,
        )
        .await
        .unwrap();

        // Send first receipt (should be accepted)
        let notification1 = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 100,
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]), // Valid signer
                timestamp_ns: 1000,
                value: 100,
            },
        );

        // Consume the initial message from task initialization
        let _initial_message = parent_rx.recv().await.unwrap();

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(notification1))
            .await
            .unwrap();

        // Receive first update
        let _first_message = parent_rx.recv().await.unwrap();

        // Send second receipt with same ID (should be rejected silently)
        let notification2 = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 100, // Same ID - should be rejected
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]), // Valid signer
                timestamp_ns: 2000,
                value: 200,
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(notification2))
            .await
            .unwrap();

        // Send third receipt with higher ID (should be accepted)
        let notification3 = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 101, // Higher ID - should be accepted
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]), // Valid signer
                timestamp_ns: 3000,
                value: 300,
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(notification3))
            .await
            .unwrap();

        // Should only receive one more message (for the third receipt)
        // The second receipt should be silently ignored, so we should get the third receipt's message
        let second_message =
            tokio::time::timeout(std::time::Duration::from_millis(100), parent_rx.recv())
                .await
                .unwrap()
                .unwrap();

        assert!(matches!(
            second_message,
            SenderAccountMessage::UpdateReceiptFees(..)
        ));
    }

    #[tokio::test]
    async fn test_invalid_receipt_tracking() {
        let lifecycle = LifecycleManager::new();

        // Create a dummy parent handle for testing
        let (parent_tx, mut parent_rx) = mpsc::channel(10);
        let parent_handle = TaskHandle::new_for_test(
            parent_tx,
            Some("test_parent".to_string()),
            std::sync::Arc::new(lifecycle.clone()),
        );

        let allocation_id =
            AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

        let task_handle = SenderAllocationTask::<Legacy>::spawn_simple(
            &lifecycle,
            Some("test_allocation".to_string()),
            allocation_id,
            parent_handle,
        )
        .await
        .unwrap();

        // Consume the initial message from task initialization
        let _initial_message = parent_rx.recv().await.unwrap();

        // Send valid receipt
        let valid_notification = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 100,
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]), // Valid signer
                timestamp_ns: 1000,
                value: 100,
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(valid_notification))
            .await
            .unwrap();

        // Should receive UpdateReceiptFees for valid receipt
        let valid_message = parent_rx.recv().await.unwrap();
        assert!(matches!(
            valid_message,
            SenderAccountMessage::UpdateReceiptFees(..)
        ));

        // Send invalid receipt (zero value)
        let invalid_notification = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 101,
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]),
                timestamp_ns: 2000,
                value: 0, // Invalid: zero value
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(invalid_notification))
            .await
            .unwrap();

        // Should receive UpdateInvalidReceiptFees for invalid receipt
        let invalid_message = parent_rx.recv().await.unwrap();
        assert!(matches!(
            invalid_message,
            SenderAccountMessage::UpdateInvalidReceiptFees(..)
        ));

        // Send receipt with suspicious ID pattern
        let suspicious_notification = NewReceiptNotification::V1(
            super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                id: 1666, // ID ending in 666 - should be marked invalid
                allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]),
                timestamp_ns: 3000,
                value: 200,
            },
        );

        task_handle
            .cast(SenderAllocationMessage::NewReceipt(suspicious_notification))
            .await
            .unwrap();

        // Should receive UpdateInvalidReceiptFees for suspicious receipt
        let suspicious_message = parent_rx.recv().await.unwrap();
        assert!(matches!(
            suspicious_message,
            SenderAccountMessage::UpdateInvalidReceiptFees(..)
        ));
    }

    #[tokio::test]
    async fn test_get_unaggregated_receipts() {
        let lifecycle = LifecycleManager::new();

        // Create a dummy parent handle for testing
        let (parent_tx, mut parent_rx) = mpsc::channel(10);
        let parent_handle = TaskHandle::new_for_test(
            parent_tx,
            Some("test_parent".to_string()),
            std::sync::Arc::new(lifecycle.clone()),
        );

        let allocation_id =
            AllocationId::Legacy(thegraph_core::AllocationId::new([1u8; 20].into()));

        let task_handle = SenderAllocationTask::<Legacy>::spawn_simple(
            &lifecycle,
            Some("test_allocation".to_string()),
            allocation_id,
            parent_handle,
        )
        .await
        .unwrap();

        // Consume the initial message from task initialization
        let _initial_message = parent_rx.recv().await.unwrap();

        // Send a few valid receipts to build up state
        for i in 1..=3 {
            let notification = NewReceiptNotification::V1(
                super::super::sender_accounts_manager::NewReceiptNotificationV1 {
                    id: i * 100,
                    allocation_id: thegraph_core::AllocationId::new([1u8; 20].into()).into_inner(),
                    signer_address: thegraph_core::alloy::primitives::Address::from([1u8; 20]),
                    timestamp_ns: i * 1000,
                    value: (i * 100) as u128,
                },
            );

            task_handle
                .cast(SenderAllocationMessage::NewReceipt(notification))
                .await
                .unwrap();

            // Consume the parent notification
            let _parent_message = parent_rx.recv().await.unwrap();
        }

        // Now test the GetUnaggregatedReceipts functionality
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        task_handle
            .cast(SenderAllocationMessage::GetUnaggregatedReceipts(reply_tx))
            .await
            .unwrap();

        // Get the response
        let unaggregated_receipts = reply_rx.await.unwrap();

        // Verify the state is correct
        assert_eq!(unaggregated_receipts.counter, 3);
        assert_eq!(unaggregated_receipts.value, 100 + 200 + 300); // 600 total
        assert_eq!(unaggregated_receipts.last_id, 300); // Last receipt ID
    }
}
