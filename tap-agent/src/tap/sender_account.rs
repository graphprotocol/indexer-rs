// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Mutex as StdMutex;
use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use alloy_sol_types::Eip712Domain;
use anyhow::{anyhow, Result};
use enum_as_inner::EnumAsInner;
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use sqlx::PgPool;
use thegraph::types::Address;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::sleep;
use tokio::{select, sync::Notify, time};
use tracing::{error, warn};

use crate::config::{self};
use crate::tap::{
    escrow_adapter::EscrowAdapter, sender_accounts_manager::NewReceiptNotification,
    sender_allocation::SenderAllocation, unaggregated_receipts::UnaggregatedReceipts,
};

#[derive(Clone, EnumAsInner)]
enum AllocationState {
    Active(Arc<SenderAllocation>),
    Ineligible(Arc<SenderAllocation>),
}

/// The inner state of a SenderAccount. This is used to store an Arc state for spawning async tasks.
pub struct Inner {
    config: &'static config::Cli,
    pgpool: PgPool,
    allocations: Arc<StdMutex<HashMap<Address, AllocationState>>>,
    sender: Address,
    sender_aggregator_endpoint: String,
    unaggregated_fees: Arc<StdMutex<UnaggregatedReceipts>>,
    unaggregated_receipts_guard: Arc<TokioMutex<()>>,
}

/// Wrapper around a `Notify` to trigger RAV requests.
/// This gives a better understanding of what is happening
/// because the methods names are more explicit.
#[derive(Clone)]
pub struct RavTrigger(Arc<Notify>);

impl RavTrigger {
    pub fn new() -> Self {
        Self(Arc::new(Notify::new()))
    }

    /// Trigger a RAV request if there are any waiters.
    /// In case there are no waiters, nothing happens.
    pub fn trigger_rav(&self) {
        self.0.notify_waiters();
    }

    /// Wait for a RAV trigger.
    pub async fn wait_for_rav_request(&self) {
        self.0.notified().await;
    }

    /// Trigger a RAV request. In case there are no waiters, the request is queued
    /// and is executed on the next call to wait_for_rav_trigger().
    pub fn trigger_next_rav(&self) {
        self.0.notify_one();
    }
}

impl Inner {
    async fn rav_requester(&self, trigger: RavTrigger) {
        loop {
            trigger.wait_for_rav_request().await;

            if let Err(error) = self.rav_requester_single().await {
                // If an error occoured, we shouldn't retry right away, so we wait for a bit.
                error!(
                    "Error while requesting RAV for sender {}: {}",
                    self.sender, error
                );
                // simpler for now, maybe we can add a backoff strategy later
                sleep(Duration::from_secs(5)).await;
                continue;
            }

            // Check if we already need to send another RAV request.
            let unaggregated_fees = self.unaggregated_fees.lock().unwrap().clone();
            if unaggregated_fees.value >= self.config.tap.rav_request_trigger_value.into() {
                // If so, "self-notify" to trigger another RAV request.
                trigger.trigger_next_rav();

                warn!(
                    "Sender {} has {} unaggregated fees immediately after a RAV request, which is
                    over the trigger value. Triggering another RAV request.",
                    self.sender, unaggregated_fees.value,
                );
            }
        }
    }

    async fn rav_requester_finalize(&self, finalize_trigger: RavTrigger) {
        loop {
            // Wait for either 5 minutes or a notification that we need to try to finalize
            // allocation receipts.
            select! {
                _ = time::sleep(Duration::from_secs(300)) => (),
                _ = finalize_trigger.wait_for_rav_request() => ()
            }

            // Get a quick snapshot of the current finalizing allocations. They are
            // Arcs, so it should be cheap.
            let allocations_finalizing = self
                .allocations
                .lock()
                .unwrap()
                .values()
                .filter(|a| matches!(a, AllocationState::Ineligible(_)))
                .map(|a| a.as_ineligible().unwrap())
                .cloned()
                .collect::<Vec<_>>();

            for allocation in allocations_finalizing {
                if let Err(e) = allocation.rav_requester_single().await {
                    error!(
                        "Error while requesting RAV for sender {} and allocation {}: {}",
                        self.sender,
                        allocation.get_allocation_id(),
                        e
                    );
                    continue;
                }

                if let Err(e) = allocation.mark_rav_final().await {
                    error!(
                        "Error while marking allocation {} as final for sender {}: {}",
                        allocation.get_allocation_id(),
                        self.sender,
                        e
                    );
                    continue;
                }

                // Remove the allocation from the finalizing map.
                self.allocations
                    .lock()
                    .unwrap()
                    .remove(&allocation.get_allocation_id());
            }
        }
    }

    /// Does a single RAV request for the sender's allocation with the highest unaggregated fees
    async fn rav_requester_single(&self) -> Result<()> {
        let heaviest_allocation = self.get_heaviest_allocation().ok_or(anyhow! {
            "Error while getting allocation with most unaggregated fees",
        })?;
        heaviest_allocation
            .rav_requester_single()
            .await
            .map_err(|e| {
                anyhow! {
                    "Error while requesting RAV for sender {} and allocation {}: {}",
                    self.sender,
                    heaviest_allocation.get_allocation_id(),
                    e
                }
            })?;

        self.recompute_unaggregated_fees().await;

        Ok(())
    }

    /// Returns the allocation with the highest unaggregated fees value.
    /// If there are no active allocations, returns None.
    fn get_heaviest_allocation(&self) -> Option<Arc<SenderAllocation>> {
        // Get a quick snapshot of all allocations. They are Arcs, so it should be cheap,
        // and we don't want to hold the lock for too long.
        let allocations: Vec<_> = self.allocations.lock().unwrap().values().cloned().collect();

        let mut heaviest_allocation = (None, 0u128);
        for allocation in allocations {
            let allocation: Arc<SenderAllocation> = match allocation {
                AllocationState::Active(a) => a,
                AllocationState::Ineligible(a) => a,
            };
            let fees = allocation.get_unaggregated_fees().value;
            if fees > heaviest_allocation.1 {
                heaviest_allocation = (Some(allocation), fees);
            }
        }

        heaviest_allocation.0
    }

    /// Recompute the sender's total unaggregated fees value and last receipt ID.
    async fn recompute_unaggregated_fees(&self) {
        // Make sure to pause the handling of receipt notifications while we update the unaggregated
        // fees.
        let _guard = self.unaggregated_receipts_guard.lock().await;

        // Similar pattern to get_heaviest_allocation().
        let allocations: Vec<_> = self.allocations.lock().unwrap().values().cloned().collect();

        // Gather the unaggregated fees from all allocations and sum them up.
        let mut unaggregated_fees = self.unaggregated_fees.lock().unwrap();
        *unaggregated_fees = UnaggregatedReceipts::default(); // Reset to 0.
        for allocation in allocations {
            let allocation: Arc<SenderAllocation> = match allocation {
                AllocationState::Active(a) => a,
                AllocationState::Ineligible(a) => a,
            };

            let uf = allocation.get_unaggregated_fees();
            *unaggregated_fees = UnaggregatedReceipts {
                value: self.fees_add(unaggregated_fees.value, uf.value),
                last_id: max(unaggregated_fees.last_id, uf.last_id),
            };
        }
    }

    /// Safe add the fees to the unaggregated fees value, log an error if there is an overflow and
    /// set the unaggregated fees value to u128::MAX.
    fn fees_add(&self, total_unaggregated_fees: u128, value_increment: u128) -> u128 {
        total_unaggregated_fees
            .checked_add(value_increment)
            .unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                error!(
                    "Overflow when adding receipt value {} to total unaggregated fees {} for \
                    sender {}. Setting total unaggregated fees to u128::MAX.",
                    value_increment, total_unaggregated_fees, self.sender
                );
                u128::MAX
            })
    }
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
    rav_trigger: RavTrigger,
    rav_requester_finalize_task: tokio::task::JoinHandle<()>,
    rav_finalize_trigger: RavTrigger,
    unaggregated_receipts_guard: Arc<TokioMutex<()>>,
}

impl SenderAccount {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        sender_id: Address,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_subgraph: &'static SubgraphClient,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoint: String,
    ) -> Self {
        let unaggregated_receipts_guard = Arc::new(TokioMutex::new(()));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone(), sender_id);

        let inner = Arc::new(Inner {
            config,
            pgpool,
            allocations: Arc::new(StdMutex::new(HashMap::new())),
            sender: sender_id,
            sender_aggregator_endpoint,
            unaggregated_fees: Arc::new(StdMutex::new(UnaggregatedReceipts::default())),
            unaggregated_receipts_guard: unaggregated_receipts_guard.clone(),
        });

        let rav_trigger = RavTrigger::new();
        let rav_requester_task = tokio::spawn({
            let inner = inner.clone();
            let rav_trigger = rav_trigger.clone();
            async move {
                inner.rav_requester(rav_trigger).await;
            }
        });

        let rav_finalize_trigger = RavTrigger::new();
        let rav_requester_finalize_task = tokio::spawn({
            let inner = inner.clone();
            let rav_finalize_trigger = rav_finalize_trigger.clone();
            async move {
                inner.rav_requester_finalize(rav_finalize_trigger).await;
            }
        });

        Self {
            inner: inner.clone(),
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            tap_eip712_domain_separator,
            rav_requester_task,
            rav_trigger,
            rav_requester_finalize_task,
            rav_finalize_trigger,
            unaggregated_receipts_guard,
        }
    }

    /// Update the sender's allocations to match the target allocations.
    pub async fn update_allocations(&self, target_allocations: HashSet<Address>) {
        {
            let mut allocations = self.inner.allocations.lock().unwrap();
            let mut allocations_to_finalize = false;

            // Make allocations that are no longer to be active `AllocationState::Ineligible`.
            for (allocation_id, allocation_state) in allocations.iter_mut() {
                if !target_allocations.contains(allocation_id) {
                    match allocation_state {
                        AllocationState::Active(allocation) => {
                            *allocation_state = AllocationState::Ineligible(allocation.clone());
                            allocations_to_finalize = true;
                        }
                        AllocationState::Ineligible(_) => {
                            // Allocation is already ineligible, do nothing.
                        }
                    }
                }
            }

            if allocations_to_finalize {
                self.rav_finalize_trigger.trigger_rav();
            }
        }

        // Add new allocations.
        for allocation_id in target_allocations {
            let sender_allocation = AllocationState::Active(Arc::new(
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
            ));
            if let std::collections::hash_map::Entry::Vacant(e) =
                self.inner.allocations.lock().unwrap().entry(allocation_id)
            {
                e.insert(sender_allocation);
            }
        }
    }

    pub async fn handle_new_receipt_notification(
        &self,
        new_receipt_notification: NewReceiptNotification,
    ) {
        // Make sure to pause the handling of receipt notifications while we update the unaggregated
        // fees.
        let _guard = self.unaggregated_receipts_guard.lock().await;

        let allocation_state = self
            .inner
            .allocations
            .lock()
            .unwrap()
            .get(&new_receipt_notification.allocation_id)
            .cloned();

        if let Some(AllocationState::Active(allocation)) = allocation_state {
            // Try to add the receipt value to the allocation's unaggregated fees value.
            // If the fees were not added, it means the receipt was already processed, so we
            // don't need to do anything.
            if allocation
                .fees_add(new_receipt_notification.value, new_receipt_notification.id)
                .await
            {
                // Add the receipt value to the allocation's unaggregated fees value.
                allocation
                    .fees_add(new_receipt_notification.value, new_receipt_notification.id)
                    .await;
                // Add the receipt value to the sender's unaggregated fees value.
                let mut unaggregated_fees = self.inner.unaggregated_fees.lock().unwrap();
                *unaggregated_fees = UnaggregatedReceipts {
                    value: self
                        .inner
                        .fees_add(unaggregated_fees.value, new_receipt_notification.value),
                    last_id: new_receipt_notification.id,
                };

                // Check if we need to trigger a RAV request.
                if unaggregated_fees.value >= self.inner.config.tap.rav_request_trigger_value.into()
                {
                    self.rav_trigger.trigger_rav();
                }
            }
        } else {
            error!(
                "Received a new receipt notification for allocation {} that doesn't exist \
                or is ineligible for sender {}.",
                new_receipt_notification.allocation_id, self.inner.sender
            );
        }
    }

    pub async fn recompute_unaggregated_fees(&self) {
        self.inner.recompute_unaggregated_fees().await
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
    use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
    use indexer_common::subgraph_client::DeploymentDetails;
    use serde_json::json;
    use std::str::FromStr;
    use tap_aggregator::server::run_server;
    use tap_core::{rav::ReceiptAggregateVoucher, signed_message::EIP712SignedMessage};
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
        pub fn _tests_get_allocations_active(&self) -> HashMap<Address, Arc<SenderAllocation>> {
            self.inner
                .allocations
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(k, v)| {
                    if let AllocationState::Active(a) = v {
                        Some((*k, a.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        }

        pub fn _tests_get_allocations_ineligible(&self) -> HashMap<Address, Arc<SenderAllocation>> {
            self.inner
                .allocations
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(k, v)| {
                    if let AllocationState::Ineligible(a) = v {
                        Some((*k, a.clone()))
                    } else {
                        None
                    }
                })
                .collect()
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

        let sender = SenderAccount::new(
            config,
            pgpool,
            SENDER.1,
            escrow_accounts_eventual,
            escrow_subgraph,
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
        sender.recompute_unaggregated_fees().await;

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
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into()).await;
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
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value,
            expected_unaggregated_fees
        );

        // Check that the sender's unaggregated fees are correct.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().unwrap().value,
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
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().unwrap().value,
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
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().unwrap().value,
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
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender.inner.unaggregated_fees.lock().unwrap().value,
            expected_unaggregated_fees
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_requester_auto(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SIGNER.0.clone(),
            vec![SIGNER.1].into_iter().collect(),
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
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, value).await;
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
            if sender_account.inner.unaggregated_fees.lock().unwrap().value < trigger_value {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT signature, allocation_id, timestamp_ns, value_aggregate
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID_0.encode_hex::<String>(),
            SENDER.1.encode_hex::<String>()
        )
        .fetch_optional(&pgpool)
        .await
        .unwrap()
        .unwrap();

        let latest_rav = EIP712SignedMessage {
            message: ReceiptAggregateVoucher {
                allocationId: Address::from_str(&latest_rav.allocation_id).unwrap(),
                timestampNs: latest_rav.timestamp_ns.to_u64().unwrap(),
                // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                // So we're converting to BigInt to get a proper implementation of to_u128().
                valueAggregate: latest_rav
                    .value_aggregate
                    .to_bigint()
                    .map(|v| v.to_u128())
                    .unwrap()
                    .unwrap(),
            },
            signature: latest_rav.signature.as_slice().try_into().unwrap(),
        };

        // Check that the latest RAV value is correct.
        assert!(latest_rav.message.valueAggregate >= trigger_value);

        // Check that the allocation's unaggregated fees value is reduced.
        assert!(
            sender_account
                .inner
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value
                <= trigger_value
        );

        // Check that the sender's unaggregated fees value is reduced.
        assert!(sender_account.inner.unaggregated_fees.lock().unwrap().value <= trigger_value);

        // Reset the total value and trigger value.
        total_value = sender_account.inner.unaggregated_fees.lock().unwrap().value;
        trigger_value = 0;

        // Add more receipts
        for i in 10..20 {
            let value = (i + 10) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into()).await;
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
            if sender_account.inner.unaggregated_fees.lock().unwrap().value < trigger_value {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT signature, allocation_id, timestamp_ns, value_aggregate
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID_0.encode_hex::<String>(),
            SENDER.1.encode_hex::<String>()
        )
        .fetch_optional(&pgpool)
        .await
        .unwrap()
        .unwrap();

        let latest_rav = EIP712SignedMessage {
            message: ReceiptAggregateVoucher {
                allocationId: Address::from_str(&latest_rav.allocation_id).unwrap(),
                timestampNs: latest_rav.timestamp_ns.to_u64().unwrap(),
                // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                // So we're converting to BigInt to get a proper implementation of to_u128().
                valueAggregate: latest_rav
                    .value_aggregate
                    .to_bigint()
                    .map(|v| v.to_u128())
                    .unwrap()
                    .unwrap(),
            },
            signature: latest_rav.signature.as_slice().try_into().unwrap(),
        };

        // Check that the latest RAV value is correct.

        assert!(latest_rav.message.valueAggregate >= trigger_value);

        // Check that the allocation's unaggregated fees value is reduced.
        assert!(
            sender_account
                .inner
                .allocations
                .lock()
                .unwrap()
                .get(&*ALLOCATION_ID_0)
                .unwrap()
                .as_active()
                .unwrap()
                .get_unaggregated_fees()
                .value
                <= trigger_value
        );

        // Check that the unaggregated fees value is reduced.
        assert!(sender_account.inner.unaggregated_fees.lock().unwrap().value <= trigger_value);

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

                    let id = sender_account
                        .inner
                        .unaggregated_fees
                        .lock()
                        .unwrap()
                        .last_id
                        + 1;

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
                        .allocations
                        .lock()
                        .unwrap()
                        .get(&allocation_id)
                        .unwrap()
                        .as_active()
                        .unwrap()
                        .get_unaggregated_fees()
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
        let heaviest_allocation = sender_account.inner.get_heaviest_allocation().unwrap();

        // Check that the heaviest allocation is correct.
        assert_eq!(heaviest_allocation.get_allocation_id(), *ALLOCATION_ID_1);

        // Check that the sender's unaggregated fees value is correct.
        assert_eq!(
            sender_account.inner.unaggregated_fees.lock().unwrap().value,
            total_value_0 + total_value_1 + total_value_2
        );
    }
}
