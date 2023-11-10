// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc};

use alloy_primitives::Address;
use alloy_sol_types::Eip712Domain;
use anyhow::ensure;
use ethereum_types::U256;
use eventuals::Eventual;
use indexer_common::prelude::SubgraphClient;
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::{
    eip_712_signed_message::EIP712SignedMessage,
    receipt_aggregate_voucher::ReceiptAggregateVoucher, tap_manager::RAVRequest,
    tap_receipt::ReceiptCheck,
};
use tokio::{
    sync::{Mutex, MutexGuard},
    task::JoinHandle,
};
use tracing::{error, warn};

use super::sender_allocation_relationships_manager::NewReceiptNotification;
use crate::{
    config::{self},
    tap::{
        escrow_adapter::EscrowAdapter, rav_storage_adapter::RAVStorageAdapter,
        receipt_checks_adapter::ReceiptChecksAdapter,
        receipt_storage_adapter::ReceiptStorageAdapter,
    },
};

type TapManager = tap_core::tap_manager::Manager<
    EscrowAdapter,
    ReceiptChecksAdapter,
    ReceiptStorageAdapter,
    RAVStorageAdapter,
>;

#[derive(Default, Debug)]
struct UnaggregatedFees {
    pub value: u128,
    /// The ID of the last receipt value added to the unaggregated fees value.
    /// This is used to make sure we don't process the same receipt twice. Relies on the fact that
    /// the receipts IDs are SERIAL in the database.
    pub last_id: u64,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum State {
    Running,
    LastRavPending,
    Finished,
}

struct Inner {
    pgpool: PgPool,
    tap_manager: TapManager,
    allocation_id: Address,
    sender: Address,
    sender_aggregator_endpoint: String,
    unaggregated_fees: Arc<Mutex<UnaggregatedFees>>,
    state: Arc<Mutex<State>>,
    config: &'static config::Cli,
}

/// A SenderAllocationRelationship is the relationship between the indexer and the sender in the
/// context of a single allocation.
///
/// Manages the lifecycle of Scalar TAP for the SenderAllocationRelationship, including:
/// - Monitoring new receipts and keeping track of the unaggregated fees.
/// - Requesting RAVs from the sender's TAP aggregator once the unaggregated fees reach a certain
///   threshold.
/// - Requesting the last RAV from the sender's TAP aggregator (on SenderAllocationRelationship EOL)
pub struct SenderAllocationRelationship {
    inner: Arc<Inner>,
    rav_requester_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl SenderAllocationRelationship {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        escrow_accounts: Eventual<HashMap<Address, U256>>,
        escrow_subgraph: &'static SubgraphClient,
        escrow_adapter: EscrowAdapter,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoint: String,
    ) -> Self {
        let required_checks = vec![
            ReceiptCheck::CheckUnique,
            ReceiptCheck::CheckAllocationId,
            ReceiptCheck::CheckTimestamp,
            // ReceiptCheck::CheckValue,
            ReceiptCheck::CheckSignature,
            ReceiptCheck::CheckAndReserveEscrow,
        ];

        let receipt_checks_adapter = ReceiptChecksAdapter::new(
            config,
            pgpool.clone(),
            // TODO: Implement query appraisals.
            None,
            allocation_id,
            escrow_accounts.clone(),
            escrow_subgraph,
            sender,
        );
        let receipt_storage_adapter = ReceiptStorageAdapter::new(
            pgpool.clone(),
            allocation_id,
            sender,
            required_checks.clone(),
        );
        let rav_storage_adapter = RAVStorageAdapter::new(pgpool.clone(), allocation_id, sender);
        let tap_manager = TapManager::new(
            tap_eip712_domain_separator.clone(),
            escrow_adapter,
            receipt_checks_adapter,
            rav_storage_adapter,
            receipt_storage_adapter,
            required_checks,
            0,
        );
        Self {
            inner: Arc::new(Inner {
                pgpool,
                tap_manager,
                allocation_id,
                sender,
                sender_aggregator_endpoint,
                unaggregated_fees: Arc::new(Mutex::new(UnaggregatedFees::default())),
                state: Arc::new(Mutex::new(State::Running)),
                config,
            }),
            rav_requester_task: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn handle_new_receipt_notification(
        &self,
        new_receipt_notification: NewReceiptNotification,
    ) {
        // If we're in the last rav pending state, we don't want to process any new receipts.
        if self.state().await == State::LastRavPending {
            error!(
                "Received a new receipt notification for allocation {} and sender {} while \
                the last RAV is pending. This should not have happened since this allocation \
                and/or sender is not eligible anymore.",
                self.inner.allocation_id, self.inner.sender
            );
            return;
        }

        let mut unaggregated_fees = self.inner.unaggregated_fees.lock().await;

        // Else we already processed that receipt, most likely from pulling the receipts
        // from the database.
        if new_receipt_notification.id > unaggregated_fees.last_id {
            unaggregated_fees.value = unaggregated_fees
                .value
                .checked_add(new_receipt_notification.value)
                .unwrap_or_else(|| {
                    // This should never happen, but if it does, we want to know about it.
                    error!(
                        "Overflow when adding receipt value {} to total unaggregated fees {} for \
                        allocation {} and sender {}. Setting total unaggregated fees to u128::MAX.",
                        new_receipt_notification.value,
                        unaggregated_fees.value,
                        new_receipt_notification.allocation_id,
                        new_receipt_notification.sender_address
                    );
                    u128::MAX
                });
            unaggregated_fees.last_id = new_receipt_notification.id;

            let mut rav_requester_task = self.rav_requester_task.lock().await;
            // TODO: consider making the trigger per sender, instead of per (sender, allocation).
            if unaggregated_fees.value >= self.inner.config.tap.rav_request_trigger_value.into()
                && !Self::rav_requester_task_is_running(&rav_requester_task)
            {
                *rav_requester_task = Some(tokio::spawn(Self::rav_requester(self.inner.clone())));
            }
        }
    }

    pub async fn start_last_rav_request(&self) {
        *(self.inner.state.lock().await) = State::LastRavPending;
        let mut rav_requester_task = self.rav_requester_task.lock().await;
        if !Self::rav_requester_task_is_running(&rav_requester_task) {
            *rav_requester_task = Some(tokio::spawn(Self::rav_requester(self.inner.clone())));
        }
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    pub async fn update_unaggregated_fees(&self) -> Result<(), anyhow::Error> {
        Self::update_unaggregated_fees_static(&self.inner).await
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn update_unaggregated_fees_static(inner: &Inner) -> Result<(), anyhow::Error> {
        inner.tap_manager.remove_obsolete_receipts().await?;

        // TODO: Get `rav.timestamp_ns` from the TAP Manager's RAV storage adapter instead?
        let res = sqlx::query!(
            r#"
            WITH rav AS (
                SELECT 
                    rav -> 'message' ->> 'timestamp_ns' AS timestamp_ns 
                FROM 
                    scalar_tap_ravs 
                WHERE 
                    allocation_id = $1 
                    AND sender_address = $2
            ) 
            SELECT 
                MAX(id), 
                SUM(value) 
            FROM 
                scalar_tap_receipts 
            WHERE 
                allocation_id = $1 
                AND sender_address = $2 
                AND CASE WHEN (
                    SELECT 
                        timestamp_ns :: NUMERIC 
                    FROM 
                        rav
                ) IS NOT NULL THEN timestamp_ns > (
                    SELECT 
                        timestamp_ns :: NUMERIC 
                    FROM 
                        rav
                ) ELSE TRUE END
            "#,
            inner
                .allocation_id
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            inner.sender.to_string().trim_start_matches("0x").to_owned()
        )
        .fetch_one(&inner.pgpool)
        .await?;

        let mut unaggregated_fees = inner.unaggregated_fees.lock().await;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        unaggregated_fees.last_id = res.max.unwrap_or(0).try_into()?;
        unaggregated_fees.value = res
            .sum
            .unwrap_or(BigDecimal::from(0))
            .to_string()
            .parse::<u128>()?;

        // TODO: check if we need to run a RAV request here.

        Ok(())
    }

    /// Request a RAV from the sender's TAP aggregator.
    /// Will remove the aggregated receipts from the database if successful.
    async fn rav_requester(inner: Arc<Inner>) {
        Self::rav_requester_try(&inner).await.unwrap_or_else(|e| {
            error!(
                "Error while requesting a RAV for allocation {} and sender {}: {:?}",
                inner.allocation_id, inner.sender, e
            );
        });
    }

    async fn rav_requester_try(inner: &Arc<Inner>) -> anyhow::Result<()> {
        loop {
            // TODO: limit the number of receipts to aggregate per request.
            let RAVRequest {
                valid_receipts,
                previous_rav,
                invalid_receipts: _,
                expected_rav,
            } = inner
                .tap_manager
                .create_rav_request(inner.config.tap.rav_request_timestamp_buffer_ns)
                .await?;

            // TODO: Request compression and response decompression. Also a fancy user agent?
            let client = HttpClientBuilder::default().build(&inner.sender_aggregator_endpoint)?;

            // TODO: Add a timeout.
            let response: JsonRpcResponse<EIP712SignedMessage<ReceiptAggregateVoucher>> = client
                .request(
                    "aggregate_receipts",
                    rpc_params!(
                        "0.0", // TODO: Set the version in a smarter place.
                        valid_receipts,
                        previous_rav
                    ),
                )
                .await?;

            if let Some(warnings) = response.warnings {
                warn!("Warnings from sender's TAP aggregator: {:?}", warnings);
            }

            inner
                .tap_manager
                .verify_and_store_rav(expected_rav, response.data)
                .await?;

            // TODO: Handle invalid receipts

            // This is not the fastest way to do this, but it's the easiest.
            // Note: we rely on the unaggregated_fees lock to make sure we don't miss any receipt
            // notifications during this.
            // TODO: If needed, faster alternative?
            Self::update_unaggregated_fees_static(inner).await?;

            let unaggregated_fees = inner.unaggregated_fees.lock().await;
            if unaggregated_fees.value < inner.config.tap.rav_request_trigger_value.into() {
                break;
            } else {
                // Go right back to requesting a RAV and warn the user.
                warn!(
                    "Unaggregated fees for allocation {} and sender {} are {} right \
                    after the RAV request. This is a sign that the TAP agent can't keep \
                    up with the rate of new receipts. Consider increasing the \
                    `rav_request_trigger_value` in the TAP agent config. It could also be \
                    a sign that the sender's TAP aggregator is too slow.",
                    inner.allocation_id, inner.sender, unaggregated_fees.value
                );
            }
        }

        let mut state = inner.state.lock().await;
        if *state == State::LastRavPending {
            // Mark the last RAV as last in the DB as a cue for the indexer-agent.
            let updated_rows = sqlx::query!(
                r#"
                    UPDATE scalar_tap_ravs
                    SET is_last = true
                    WHERE allocation_id = $1 AND sender_address = $2
                    RETURNING *
                "#,
                inner
                    .allocation_id
                    .to_string()
                    .trim_start_matches("0x")
                    .to_owned(),
                inner.sender.to_string().trim_start_matches("0x").to_owned(),
            )
            .fetch_all(&inner.pgpool)
            .await?;

            // Make sure exactly one row was updated.
            if updated_rows.len() != 1 {
                anyhow::bail!(
                    "Expected exactly one row to be updated in the latest RAVs table, \
                    but {} were updated.",
                    updated_rows.len()
                );
            }

            *state = State::Finished;
        };
        anyhow::Ok(())
    }

    fn rav_requester_task_is_running(
        rav_requester_task_lock: &MutexGuard<'_, Option<JoinHandle<()>>>,
    ) -> bool {
        if let Some(handle) = rav_requester_task_lock.as_ref() {
            !handle.is_finished()
        } else {
            false
        }
    }

    pub async fn state(&self) -> State {
        *self.inner.state.lock().await
    }
}

impl Drop for SenderAllocationRelationship {
    /// Trying to make sure the RAV requester task is dropped when the SenderAllocationRelationship
    /// is dropped.
    fn drop(&mut self) {
        let rav_requester_task = self.rav_requester_task.clone();

        tokio::spawn(async move {
            let mut rav_requester_task = rav_requester_task.lock().await;
            if let Some(rav_requester_task) = rav_requester_task.take() {
                rav_requester_task.abort();
            }
        });
    }
}

#[cfg(test)]
mod tests {

    use indexer_common::subgraph_client::DeploymentDetails;
    use serde_json::json;
    use tap_aggregator::server::run_server;
    use tap_core::tap_manager::SignedRAV;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::tap::test_utils::{
        create_rav, create_received_receipt, store_rav, store_receipt, ALLOCATION_ID, INDEXER,
        SENDER, TAP_EIP712_DOMAIN_SEPARATOR,
    };

    const DUMMY_URL: &str = "http://localhost:1234";

    async fn create_sender_allocation_relationship(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
    ) -> SenderAllocationRelationship {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
                ..Default::default()
            },
            tap: config::Tap {
                rav_request_trigger_value: 100,
                rav_request_timestamp_buffer_ns: 1000,
                ..Default::default()
            },
            ..Default::default()
        }));

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
        )));

        let (mut escrow_accounts_writer, escrow_accounts_eventual) =
            Eventual::<HashMap<Address, U256>>::new();
        escrow_accounts_writer.write(HashMap::from([(SENDER.1, 1000.into())]));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone());

        SenderAllocationRelationship::new(
            config,
            pgpool.clone(),
            *ALLOCATION_ID,
            SENDER.1,
            escrow_accounts_eventual,
            escrow_subgraph,
            escrow_adapter,
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint,
        )
    }

    /// Test that the sender_allocation_relatioship correctly updates the unaggregated fees from the
    /// database when there is no RAV in the database.
    ///
    /// The sender_allocation_relatioship should consider all receipts found for the allocation and
    /// sender.
    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_unaggregated_fees_no_rav(pgpool: PgPool) {
        let sender_allocation_relatioship =
            create_sender_allocation_relationship(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL)
                .await;

        // Add receipts to the database.
        for i in 1..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation_relatioship update the unaggregated fees from the database.
        sender_allocation_relatioship
            .update_unaggregated_fees()
            .await
            .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            45u128
        );
    }

    /// Test that the sender_allocation_relatioship correctly updates the unaggregated fees from the
    /// database when there is a RAV in the database as well as receipts which timestamp are lesser
    /// and greater than the RAV's timestamp.
    ///
    /// The sender_allocation_relatioship should only consider receipts with a timestamp greater
    /// than the RAV's timestamp.
    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_unaggregated_fees_with_rav(pgpool: PgPool) {
        let sender_allocation_relatioship =
            create_sender_allocation_relationship(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL)
                .await;

        // Add the RAV to the database.
        // This RAV has timestamp 4. The sender_allocation_relatioship should only consider receipts
        // with a timestamp greater than 4.
        let signed_rav = create_rav(*ALLOCATION_ID, SENDER.0.clone(), 4, 10).await;
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation_relatioship update the unaggregated fees from the database.
        sender_allocation_relatioship
            .update_unaggregated_fees()
            .await
            .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            35u128
        );
    }

    /// Test that the sender_allocation_relatioship correctly ignores new receipt notifications with
    /// an ID lower than the last receipt ID processed (be it from the DB or from a prior receipt
    /// notification).
    #[sqlx::test(migrations = "../migrations")]
    async fn test_handle_new_receipt_notification(pgpool: PgPool) {
        let sender_allocation_relatioship =
            create_sender_allocation_relationship(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL)
                .await;

        // Add receipts to the database.
        let mut expected_unaggregated_fees = 0u128;
        for i in 10..20 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
            expected_unaggregated_fees += u128::from(i);
        }

        sender_allocation_relatioship
            .update_unaggregated_fees()
            .await
            .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification that has a lower ID than the last loaded from the DB.
        // The last ID in the DB should be 10, since we added 10 receipts to the empty receipts
        // table
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID,
            sender_address: SENDER.1,
            id: 10,
            timestamp_ns: 19,
            value: 19,
        };
        sender_allocation_relatioship
            .handle_new_receipt_notification(new_receipt_notification)
            .await;

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification.
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID,
            sender_address: SENDER.1,
            id: 30,
            timestamp_ns: 20,
            value: 20,
        };
        sender_allocation_relatioship
            .handle_new_receipt_notification(new_receipt_notification)
            .await;
        expected_unaggregated_fees += 20;

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            expected_unaggregated_fees
        );

        // Send a new receipt notification that has a lower ID than the previous one.
        let new_receipt_notification = NewReceiptNotification {
            allocation_id: *ALLOCATION_ID,
            sender_address: SENDER.1,
            id: 25,
            timestamp_ns: 19,
            value: 19,
        };
        sender_allocation_relatioship
            .handle_new_receipt_notification(new_receipt_notification)
            .await;

        // Check that the unaggregated fees have *not* increased.
        assert_eq!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value,
            expected_unaggregated_fees
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_requester_manual(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SENDER.0.clone(),
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

        // Create a sender_allocation_relatioship.
        let sender_allocation_relatioship = create_sender_allocation_relationship(
            pgpool.clone(),
            "http://".to_owned() + &aggregator_endpoint.to_string(),
            &mock_server.uri(),
        )
        .await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i + 1, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation_relatioship update the unaggregated fees from the database.
        sender_allocation_relatioship
            .update_unaggregated_fees()
            .await
            .unwrap();

        // Trigger a RAV request manually.
        SenderAllocationRelationship::rav_requester_try(&sender_allocation_relatioship.inner)
            .await
            .unwrap();

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_requester_auto(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SENDER.0.clone(),
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

        // Create a sender_allocation_relatioship.
        let sender_allocation_relatioship = create_sender_allocation_relationship(
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
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i + 1, value, i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
            sender_allocation_relatioship
                .handle_new_receipt_notification(NewReceiptNotification {
                    allocation_id: *ALLOCATION_ID,
                    sender_address: SENDER.1,
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
        while SenderAllocationRelationship::rav_requester_task_is_running(
            &sender_allocation_relatioship
                .rav_requester_task
                .lock()
                .await,
        ) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT rav
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            SENDER.1.to_string().trim_start_matches("0x").to_owned()
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

        // Check that the unaggregated fees value is reduced.
        assert!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value
                <= trigger_value
        );

        // Reset the total value and trigger value.
        total_value = sender_allocation_relatioship
            .inner
            .unaggregated_fees
            .lock()
            .await
            .value;
        trigger_value = 0;

        // Add more receipts
        for i in 10..20 {
            let value = (i + 10) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID, &SENDER.0, i, i + 1, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();

            sender_allocation_relatioship
                .handle_new_receipt_notification(NewReceiptNotification {
                    allocation_id: *ALLOCATION_ID,
                    sender_address: SENDER.1,
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
        while SenderAllocationRelationship::rav_requester_task_is_running(
            &sender_allocation_relatioship
                .rav_requester_task
                .lock()
                .await,
        ) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Get the latest RAV from the database.
        let latest_rav = sqlx::query!(
            r#"
                SELECT rav
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            ALLOCATION_ID
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            SENDER.1.to_string().trim_start_matches("0x").to_owned()
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

        // Check that the unaggregated fees value is reduced.
        assert!(
            sender_allocation_relatioship
                .inner
                .unaggregated_fees
                .lock()
                .await
                .value
                <= trigger_value
        );

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }
}
