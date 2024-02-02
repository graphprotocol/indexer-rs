// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use alloy_sol_types::Eip712Domain;
use anyhow::{anyhow, Result};
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use sqlx::PgPool;
use thegraph::types::Address;
use tokio::{
    select,
    sync::{Mutex, MutexGuard, Notify},
    task::JoinSet,
    time,
};
use tracing::{error, warn};

use crate::config::{self};
use crate::tap::{
    escrow_adapter::EscrowAdapter, sender_accounts_manager::NewReceiptNotification,
    sender_allocation::SenderAllocation, unaggregated_receipts::UnaggregatedReceipts,
};

pub struct Inner {
    config: &'static config::Cli,
    pgpool: PgPool,
    allocations_active: Mutex<HashMap<Address, Arc<SenderAllocation>>>,
    allocations_ineligible: Mutex<HashMap<Address, Arc<SenderAllocation>>>,
    sender: Address,
    sender_aggregator_endpoint: String,
    unaggregated_fees: Mutex<UnaggregatedReceipts>,
}

/// A SenderAccount manages the receipts accounting between the indexer and the sender across
/// multiple allocations.
///
/// Manages the lifecycle of Scalar TAP for the SenderAccount, including:
/// - Monitoring new receipts and keeping track of the cumulative unaggregated fees across
///   allocations.
/// - Requesting RAVs from the sender's TAP aggregator once the cumulative unaggregated fees reach a
///   certain threshold.
/// - Requesting the last RAV from the sender's TAP aggregator for all EOL allocations.
pub struct SenderAccount {
    inner: Arc<Inner>,
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    tap_eip712_domain_separator: Eip712Domain,
    rav_requester_task: tokio::task::JoinHandle<()>,
    rav_requester_notify: Arc<Notify>,
    rav_requester_finalize_task: tokio::task::JoinHandle<()>,
    rav_requester_finalize_notify: Arc<Notify>,
}

impl SenderAccount {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        sender_id: Address,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_subgraph: &'static SubgraphClient,
        escrow_adapter: EscrowAdapter,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoint: String,
    ) -> Self {
        let inner = Arc::new(Inner {
            config,
            pgpool,
            allocations_active: Mutex::new(HashMap::new()),
            allocations_ineligible: Mutex::new(HashMap::new()),
            sender: sender_id,
            sender_aggregator_endpoint,
            unaggregated_fees: Mutex::new(UnaggregatedReceipts::default()),
        });

        let rav_requester_notify = Arc::new(Notify::new());
        let rav_requester_finalize_notify = Arc::new(Notify::new());

        Self {
            inner: inner.clone(),
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            tap_eip712_domain_separator,
            rav_requester_task: tokio::spawn(Self::rav_requester(
                inner.clone(),
                rav_requester_notify.clone(),
            )),
            rav_requester_notify,
            rav_requester_finalize_task: tokio::spawn(Self::rav_requester_finalize(
                inner.clone(),
                rav_requester_finalize_notify.clone(),
            )),
            rav_requester_finalize_notify,
        }
    }

    /// Update the sender's allocations to match the target allocations.
    pub async fn update_allocations(&self, target_allocations: HashSet<Address>) {
        let mut allocations_active = self.inner.allocations_active.lock().await;
        let mut allocations_ineligible = self.inner.allocations_ineligible.lock().await;

        // Move allocations that are no longer to be active to the ineligible map.
        let mut allocations_to_move = Vec::new();
        for allocation_id in allocations_active.keys() {
            if !target_allocations.contains(allocation_id) {
                allocations_to_move.push(*allocation_id);
            }
        }
        for allocation_id in &allocations_to_move {
            allocations_ineligible.insert(
                *allocation_id,
                allocations_active.remove(allocation_id).unwrap(),
            );
        }

        // If we moved any allocations to the ineligible map, notify the RAV requester finalize
        // task.
        if !allocations_to_move.is_empty() {
            self.rav_requester_finalize_notify.notify_waiters();
        }

        // Add new allocations.
        for allocation_id in target_allocations {
            if !allocations_active.contains_key(&allocation_id)
                && !allocations_ineligible.contains_key(&allocation_id)
            {
                allocations_active.insert(
                    allocation_id,
                    Arc::new(
                        SenderAllocation::new(
                            self.inner.config,
                            self.inner.pgpool.clone(),
                            allocation_id,
                            self.inner.sender,
                            self.escrow_accounts.clone(),
                            self.escrow_subgraph,
                            self.escrow_adapter.clone(),
                            self.tap_eip712_domain_separator.clone(),
                            self.inner.sender_aggregator_endpoint.clone(),
                        )
                        .await,
                    ),
                );
            }
        }
    }

    pub async fn handle_new_receipt_notification(
        &self,
        new_receipt_notification: NewReceiptNotification,
    ) {
        let mut unaggregated_fees = self.inner.unaggregated_fees.lock().await;

        // Else we already processed that receipt, most likely from pulling the receipts
        // from the database.
        if new_receipt_notification.id > unaggregated_fees.last_id {
            if let Some(allocation) = self
                .inner
                .allocations_active
                .lock()
                .await
                .get(&new_receipt_notification.allocation_id)
            {
                // Add the receipt value to the allocation's unaggregated fees value.
                allocation.fees_add(new_receipt_notification.value).await;
                // Add the receipt value to the sender's unaggregated fees value.
                Self::fees_add(
                    self.inner.clone(),
                    &mut unaggregated_fees,
                    new_receipt_notification.value,
                );

                unaggregated_fees.last_id = new_receipt_notification.id;

                // Check if we need to trigger a RAV request.
                if unaggregated_fees.value >= self.inner.config.tap.rav_request_trigger_value.into()
                {
                    self.rav_requester_notify.notify_waiters();
                }
            } else {
                error!(
                    "Received a new receipt notification for allocation {} that doesn't exist \
                    or is ineligible for sender {}.",
                    new_receipt_notification.allocation_id, self.inner.sender
                );
            }
        }
    }

    async fn rav_requester(inner: Arc<Inner>, notif_value_trigger: Arc<Notify>) {
        loop {
            notif_value_trigger.notified().await;

            Self::rav_requester_single(inner.clone()).await;

            // Check if we already need to send another RAV request.
            let unaggregated_fees = inner.unaggregated_fees.lock().await;
            if unaggregated_fees.value >= inner.config.tap.rav_request_trigger_value.into() {
                // If so, "self-notify" to trigger another RAV request.
                notif_value_trigger.notify_one();

                warn!(
                    "Sender {} has {} unaggregated fees immediately after a RAV request, which is
                    over the trigger value. Triggering another RAV request.",
                    inner.sender, unaggregated_fees.value,
                );
            }
        }
    }

    async fn rav_requester_finalize(inner: Arc<Inner>, notif_finalize_allocations: Arc<Notify>) {
        loop {
            // Wait for either 5 minutes or a notification that we need to try to finalize
            // allocation receipts.
            select! {
                _ = time::sleep(Duration::from_secs(300)) => (),
                _ = notif_finalize_allocations.notified() => ()
            }

            // Get a quick snapshot of the current finalizing allocations. They are
            // Arcs, so it should be cheap.
            let allocations_finalizing = inner
                .allocations_ineligible
                .lock()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();

            for allocation in allocations_finalizing {
                if let Err(e) = allocation.rav_requester_single().await {
                    error!(
                        "Error while requesting RAV for sender {} and allocation {}: {}",
                        inner.sender,
                        allocation.get_allocation_id(),
                        e
                    );
                    continue;
                }

                if let Err(e) = allocation.mark_rav_final().await {
                    error!(
                        "Error while marking allocation {} as final for sender {}: {}",
                        allocation.get_allocation_id(),
                        inner.sender,
                        e
                    );
                    continue;
                }

                // Remove the allocation from the finalizing map.
                inner
                    .allocations_ineligible
                    .lock()
                    .await
                    .remove(&allocation.get_allocation_id());
            }
        }
    }

    /// Does a single RAV request for the sender's allocation with the highest unaggregated fees
    async fn rav_requester_single(inner: Arc<Inner>) {
        let heaviest_allocation = match Self::get_heaviest_allocation(inner.clone()).await {
            Ok(a) => a,
            Err(e) => {
                error!(
                    "Error while getting allocation with most unaggregated fees: {}",
                    e
                );
                return;
            }
        };

        if let Err(e) = heaviest_allocation.rav_requester_single().await {
            error!(
                "Error while requesting RAV for sender {} and allocation {}: {}",
                inner.sender,
                heaviest_allocation.get_allocation_id(),
                e
            );
            return;
        };

        if let Err(e) = Self::recompute_unaggregated_fees_static(inner.clone()).await {
            error!(
                "Error while recomputing unaggregated fees for sender {}: {}",
                inner.sender, e
            );
        }
    }

    /// Returns the allocation with the highest unaggregated fees value.
    async fn get_heaviest_allocation(inner: Arc<Inner>) -> Result<Arc<SenderAllocation>> {
        // Get a quick snapshot of the current allocations. They are Arcs, so it should be cheap,
        // and we don't want to hold the lock for too long.
        let allocations: Vec<_> = inner
            .allocations_active
            .lock()
            .await
            .values()
            .cloned()
            .collect();

        // Get the fees for each allocation in parallel. This is required because the
        // SenderAllocation's fees is behind a Mutex.
        let mut set = JoinSet::new();
        for allocation in allocations {
            set.spawn(async move {
                (
                    allocation.clone(),
                    allocation.get_unaggregated_fees().await.value,
                )
            });
        }

        // Find the allocation with the highest fees. Doing it "manually" because we can't get an
        // iterator from the JoinSet, and collecting into a Vec doesn't make it much simpler
        // anyway.
        let mut heaviest_allocation = (None, 0u128);
        while let Some(res) = set.join_next().await {
            let (allocation, fees) = res?;
            if fees > heaviest_allocation.1 {
                heaviest_allocation = (Some(allocation), fees);
            }
        }

        heaviest_allocation
            .0
            .ok_or(anyhow!("Heaviest allocation is None"))
    }

    pub async fn recompute_unaggregated_fees(&self) -> Result<()> {
        Self::recompute_unaggregated_fees_static(self.inner.clone()).await
    }

    /// Recompute the sender's total unaggregated fees value and last receipt ID.
    async fn recompute_unaggregated_fees_static(inner: Arc<Inner>) -> Result<()> {
        // Similar pattern to get_heaviest_allocation().
        let allocations: Vec<_> = inner
            .allocations_active
            .lock()
            .await
            .values()
            .cloned()
            .collect();

        let mut set = JoinSet::new();
        for allocation in allocations {
            set.spawn(async move { allocation.get_unaggregated_fees().await });
        }

        // Added benefit to this lock is that it pauses the handle_new_receipt_notification() calls
        // while we're recomputing the unaggregated fees value. Hopefully this is a very short
        // pause overall.
        let mut unaggregated_fees = inner.unaggregated_fees.lock().await;

        // Recompute the sender's total unaggregated fees value and last receipt ID, because new
        // receipts might have been added to the DB in the meantime.
        *unaggregated_fees = UnaggregatedReceipts::default(); // Reset to 0.
        while let Some(uf) = set.join_next().await {
            let uf = uf?;
            Self::fees_add(inner.clone(), &mut unaggregated_fees, uf.value);
            unaggregated_fees.last_id = max(unaggregated_fees.last_id, uf.last_id);
        }

        Ok(())
    }

    /// Safe add the fees to the unaggregated fees value, log an error if there is an overflow and
    /// set the unaggregated fees value to u128::MAX.
    fn fees_add(
        inner: Arc<Inner>,
        unaggregated_fees: &mut MutexGuard<'_, UnaggregatedReceipts>,
        value: u128,
    ) {
        unaggregated_fees.value = unaggregated_fees
            .value
            .checked_add(value)
            .unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                error!(
                    "Overflow when adding receipt value {} to total unaggregated fees {} for \
                    sender {}. Setting total unaggregated fees to u128::MAX.",
                    value, unaggregated_fees.value, inner.sender
                );
                u128::MAX
            });
    }
}

// Abort tasks on Drop
impl Drop for SenderAccount {
    fn drop(&mut self) {
        self.rav_requester_task.abort();
        self.rav_requester_finalize_task.abort();
    }
}

#[cfg(test)]
mod tests {

    use alloy_primitives::hex::ToHex;
    use indexer_common::subgraph_client::DeploymentDetails;
    use serde_json::json;
    use tap_aggregator::server::run_server;
    use tap_core::tap_manager::SignedRAV;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use crate::tap::test_utils::{
        create_received_receipt, store_receipt, ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2,
        INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    };

    use super::*;

    const DUMMY_URL: &str = "http://localhost:1234";

    // To help with testing from other modules.
    impl SenderAccount {
        pub async fn _tests_get_allocations_active(
            &self,
        ) -> MutexGuard<'_, HashMap<Address, Arc<SenderAllocation>>> {
            self.inner.allocations_active.lock().await
        }

        pub async fn _tests_get_allocations_ineligible(
            &self,
        ) -> MutexGuard<'_, HashMap<Address, Arc<SenderAllocation>>> {
            self.inner.allocations_ineligible.lock().await
        }
    }

    async fn create_sender_with_allocations(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
    ) -> SenderAccount {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: 100,
                rav_request_timestamp_buffer_ms: 1,
                rav_request_timeout_secs: 5,
                ..Default::default()
            },
            ..Default::default()
        }));

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
        )));

        let escrow_accounts_eventual = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone());

        let sender = SenderAccount::new(
            config,
            pgpool,
            SENDER.1,
            escrow_accounts_eventual,
            escrow_subgraph,
            escrow_adapter,
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint,
        );

        sender
            .update_allocations(HashSet::from([
                *ALLOCATION_ID_0,
                *ALLOCATION_ID_1,
                *ALLOCATION_ID_2,
            ]))
            .await;
        sender.recompute_unaggregated_fees().await.unwrap();

        sender
    }

    /// Test that the sender_account correctly ignores new receipt notifications with
    /// an ID lower than the last receipt ID processed (be it from the DB or from a prior receipt
    /// notification).
    #[sqlx::test(migrations = "../migrations")]
    async fn test_handle_new_receipt_notification(pgpool: PgPool) {
        // Add receipts to the database. Before creating the sender and allocation so that it loads
        // the receipts from the DB.
        let mut expected_unaggregated_fees = 0u128;
        for i in 10..20 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
            expected_unaggregated_fees += u128::from(i);
        }

        let sender =
            create_sender_with_allocations(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

        // Check that the allocation's unaggregated fees are correct.
        assert_eq!(
            sender
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Check that the sender's unaggregated fees are correct.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().await.value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification that has a lower ID than the last loaded from the DB.
        // The last ID in the DB should be 10, since we added 10 receipts to the empty receipts
        // table
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            id: 10,
            timestamp_ns: 19,
            value: 19,
        };
        sender
            .handle_new_receipt_notification(new_receipt_notification)
            .await;

        // Check that the allocation's unaggregated fees have *not* increased.
        assert_eq!(
            sender
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().await.value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification.
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            id: 30,
            timestamp_ns: 20,
            value: 20,
        };
        sender
            .handle_new_receipt_notification(new_receipt_notification)
            .await;
        expected_unaggregated_fees += 20;

        // Check that the allocation's unaggregated fees are correct.
        assert_eq!(
            sender
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().await.value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification that has a lower ID than the previous one.
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            id: 25,
            timestamp_ns: 19,
            value: 19,
        };
        sender
            .handle_new_receipt_notification(new_receipt_notification)
            .await;

        // Check that the allocation's unaggregated fees have *not* increased.
        assert_eq!(
            sender
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().await.value,
            expected_unaggregated_fees
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_requester_auto(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SIGNER.0.clone(),
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            100 * 1024,
            100 * 1024,
            1,
        )
        .await
        .unwrap();

        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        // Create a sender_account.
        let sender_account = create_sender_with_allocations(
            pgpool.clone(),
            "http://".to_owned() + &aggregator_endpoint.to_string(),
            &mock_server.uri(),
        )
        .await;

        // Add receipts to the database and call the `handle_new_receipt_notification` method
        // correspondingly.
        let mut total_value = 0;
        let mut trigger_value = 0;
        for i in 0..10 {
            // These values should be enough to trigger a RAV request at i == 7 since we set the
            // `rav_request_trigger_value` to 100.
            let value = (i + 10) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, value, i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
            sender_account
                .handle_new_receipt_notification(NewReceiptNotification {
                    allocation_id: *ALLOCATION_ID_0,
                    signer_address: SIGNER.1,
                    id: i,
                    timestamp_ns: i + 1,
                    value,
                })
                .await;

            total_value += value;
            if total_value >= 100 && trigger_value == 0 {
                trigger_value = total_value;
            }
        }

        // Wait for the RAV requester to finish.
        for _ in 0..100 {
            if sender_account.inner.unaggregated_fees.lock().await.value < trigger_value {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT rav
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID_0.encode_hex::<String>(),
            SENDER.1.encode_hex::<String>()
        )
        .fetch_optional(&pgpool)
        .await
        .map(|r| r.map(|r| r.rav))
        .unwrap();

        let latest_rav = latest_rav
            .map(|r| serde_json::from_value::<SignedRAV>(r).unwrap())
            .unwrap();

        // Check that the latest RAV value is correct.
        assert!(latest_rav.message.value_aggregate >= trigger_value);

        // Check that the allocation's unaggregated fees value is reduced.
        assert!(
            sender_account
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value
                <= trigger_value
        );

        // Check that the sender's unaggregated fees value is reduced.
        assert!(sender_account.inner.unaggregated_fees.lock().await.value <= trigger_value);

        // Reset the total value and trigger value.
        total_value = sender_account.inner.unaggregated_fees.lock().await.value;
        trigger_value = 0;

        // Add more receipts
        for i in 10..20 {
            let value = (i + 10) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();

            sender_account
                .handle_new_receipt_notification(NewReceiptNotification {
                    allocation_id: *ALLOCATION_ID_0,
                    signer_address: SIGNER.1,
                    id: i,
                    timestamp_ns: i + 1,
                    value,
                })
                .await;

            total_value += value;
            if total_value >= 100 && trigger_value == 0 {
                trigger_value = total_value;
            }
        }

        // Wait for the RAV requester to finish.
        for _ in 0..100 {
            if sender_account.inner.unaggregated_fees.lock().await.value < trigger_value {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT rav
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID_0.encode_hex::<String>(),
            SENDER.1.encode_hex::<String>()
        )
        .fetch_optional(&pgpool)
        .await
        .map(|r| r.map(|r| r.rav))
        .unwrap();

        let latest_rav = latest_rav
            .map(|r| serde_json::from_value::<SignedRAV>(r).unwrap())
            .unwrap();

        // Check that the latest RAV value is correct.

        assert!(latest_rav.message.value_aggregate >= trigger_value);

        // Check that the allocation's unaggregated fees value is reduced.
        assert!(
            sender_account
                .inner
                .allocations_active
                .lock()
                .await
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .get_unaggregated_fees()
                .await
                .value
                <= trigger_value
        );

        // Check that the unaggregated fees value is reduced.
        assert!(sender_account.inner.unaggregated_fees.lock().await.value <= trigger_value);

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_sender_unaggregated_fees(pgpool: PgPool) {
        // Create a sender_account.
        let sender_account = Arc::new(
            create_sender_with_allocations(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await,
        );

        // Closure that adds a number of receipts to an allocation.
        let add_receipts = |allocation_id: Address, iterations: u64| {
            let sender_account = sender_account.clone();

            async move {
                let mut total_value = 0;
                for i in 0..iterations {
                    let value = (i + 10) as u128;

                    let id = sender_account.inner.unaggregated_fees.lock().await.last_id + 1;

                    sender_account
                        .handle_new_receipt_notification(NewReceiptNotification {
                            allocation_id,
                            signer_address: SIGNER.1,
                            id,
                            timestamp_ns: i + 1,
                            value,
                        })
                        .await;

                    total_value += value;
                }

                assert_eq!(
                    sender_account
                        .inner
                        .allocations_active
                        .lock()
                        .await
                        .get(&allocation_id)
                        .unwrap()
                        .get_unaggregated_fees()
                        .await
                        .value,
                    total_value
                );

                total_value
            }
        };

        // Add receipts to the database for allocation_0
        let total_value_0 = add_receipts(*ALLOCATION_ID_0, 9).await;

        // Add receipts to the database for allocation_1
        let total_value_1 = add_receipts(*ALLOCATION_ID_1, 10).await;

        // Add receipts to the database for allocation_2
        let total_value_2 = add_receipts(*ALLOCATION_ID_2, 8).await;

        // Get the heaviest allocation.
        let heaviest_allocation =
            SenderAccount::get_heaviest_allocation(sender_account.inner.clone())
                .await
                .unwrap();

        // Check that the heaviest allocation is correct.
        assert_eq!(heaviest_allocation.get_allocation_id(), *ALLOCATION_ID_1);

        // Check that the sender's unaggregated fees value is correct.
        assert_eq!(
            sender_account.inner.unaggregated_fees.lock().await.value,
            total_value_0 + total_value_1 + total_value_2
        );
    }
}
