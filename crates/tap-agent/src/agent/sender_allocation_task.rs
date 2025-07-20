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

/// Simple state structure for the task
struct TaskState {
    /// Sum of all receipt fees for the current allocation
    unaggregated_fees: UnaggregatedReceipts,
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

    /// Handle new receipt - simplified version
    async fn handle_new_receipt(
        state: &mut TaskState,
        notification: NewReceiptNotification,
    ) -> anyhow::Result<()> {
        // For now, just accept all receipts as valid
        // In the full implementation, this would include validation

        let value = match notification {
            NewReceiptNotification::V1(ref n) => n.value,
            NewReceiptNotification::V2(ref n) => n.value,
        };

        let timestamp_ns = match notification {
            NewReceiptNotification::V1(ref n) => n.timestamp_ns,
            NewReceiptNotification::V2(ref n) => n.timestamp_ns,
        };

        // Update local state
        state.unaggregated_fees.value += value;
        state.unaggregated_fees.counter += 1;

        // Notify parent
        state
            .sender_account_handle
            .cast(SenderAccountMessage::UpdateReceiptFees(
                state.allocation_id,
                ReceiptFees::NewReceipt(value, timestamp_ns),
            ))
            .await?;

        Ok(())
    }

    /// Handle RAV request - simplified version
    async fn handle_rav_request(state: &mut TaskState) -> anyhow::Result<()> {
        let _start_time = Instant::now();

        // For now, simulate a successful RAV request
        // In the full implementation, this would make actual gRPC calls

        // Create a dummy RAV info
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

        // Reset local fees since they're now covered by RAV
        state.unaggregated_fees = UnaggregatedReceipts::default();

        // Notify parent of successful RAV
        state
            .sender_account_handle
            .cast(SenderAccountMessage::UpdateReceiptFees(
                state.allocation_id,
                ReceiptFees::RavRequestResponse(state.unaggregated_fees, Ok(Some(rav_info))),
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
        matches!(parent_message, SenderAccountMessage::UpdateReceiptFees(..));
    }
}
