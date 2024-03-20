// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};

use alloy_sol_types::Eip712Domain;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::allocations::Allocation;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use ractor::{call, Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sqlx::PgPool;
use thegraph::types::Address;
use tracing::error;

use super::sender_allocation::SenderAllocation;
use crate::agent::allocation_id_tracker::AllocationIdTracker;
use crate::agent::sender_allocation::SenderAllocationMsg;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::escrow_adapter::EscrowAdapter,
};

pub enum SenderAccountMessage {
    CreateSenderAllocation(Address),
    UpdateAllocationIds(HashSet<Address>),
    RemoveSenderAccount,
    UpdateReceiptFees(Address, UnaggregatedReceipts),
    GetAllocationTracker(RpcReplyPort<AllocationIdTracker>),
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
    //Eventuals
    escrow_accounts: Eventual<EscrowAccounts>,
    indexer_allocations: Eventual<HashSet<Address>>,

    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    tap_eip712_domain_separator: Eip712Domain,
    config: &'static config::Cli,
    pgpool: PgPool,
    sender: Address,
    sender_aggregator_endpoint: String,
}

pub struct State {
    allocation_id_tracker: AllocationIdTracker,
    allocation_ids: HashSet<Address>,
    _indexer_allocations_handle: PipeHandle,
}

#[async_trait::async_trait]
impl Actor for SenderAccount {
    type Msg = SenderAccountMessage;
    type State = State;
    type Arguments = HashSet<Address>;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        allocation_ids: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let clone = myself.clone();
        let _indexer_allocations_handle =
            self.indexer_allocations
                .clone()
                .pipe_async(move |allocation_ids| {
                    let myself = clone.clone();
                    async move {
                        // Update the allocation_ids
                        myself
                            .cast(SenderAccountMessage::UpdateAllocationIds(allocation_ids))
                            .unwrap_or_else(|e| {
                                error!("Error while updating allocation_ids: {:?}", e);
                            });
                    }
                });

        for allocation_id in &allocation_ids {
            // Create a sender allocation for each allocation
            myself.cast(SenderAccountMessage::CreateSenderAllocation(*allocation_id))?;
        }

        Ok(State {
            allocation_id_tracker: AllocationIdTracker::new(),
            allocation_ids,
            _indexer_allocations_handle,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match message {
            SenderAccountMessage::RemoveSenderAccount => myself.stop(None),
            SenderAccountMessage::UpdateReceiptFees(allocation_id, unaggregated_fees) => {
                let tracker = &mut state.allocation_id_tracker;
                tracker.add_or_update(allocation_id, unaggregated_fees.value);

                if tracker.get_total_fee() >= self.config.tap.rav_request_trigger_value.into() {
                    self.rav_requester_single(tracker).await?;
                }
            }
            SenderAccountMessage::CreateSenderAllocation(allocation_id) => {
                let sender_allocation = SenderAllocation::new(
                    self.config,
                    self.pgpool.clone(),
                    allocation_id,
                    self.sender,
                    self.escrow_accounts.clone(),
                    self.escrow_subgraph,
                    self.escrow_adapter.clone(),
                    self.tap_eip712_domain_separator.clone(),
                    self.sender_aggregator_endpoint.clone(),
                    myself.clone(),
                )
                .await;
                let sender_id = self.sender;

                let (_actor, _handle) = SenderAllocation::spawn_linked(
                    Some(format!("{sender_id}:{allocation_id}")),
                    sender_allocation,
                    (),
                    myself.get_cell(),
                )
                .await?;
            }
            SenderAccountMessage::UpdateAllocationIds(allocation_ids) => {
                // Create new sender allocations
                for allocation_id in allocation_ids.difference(&state.allocation_ids) {
                    myself.cast(SenderAccountMessage::CreateSenderAllocation(*allocation_id))?;
                }

                // Remove sender allocations
                let sender = self.sender;
                for allocation_id in state.allocation_ids.difference(&allocation_ids) {
                    if let Some(sender_handle) = ActorRef::<SenderAllocationMsg>::where_is(format!(
                        "{sender}:{allocation_id}"
                    )) {
                        sender_handle.cast(SenderAllocationMsg::CloseAllocation)?;
                    }
                }

                state.allocation_ids = allocation_ids;
            }
            SenderAccountMessage::GetAllocationTracker(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.allocation_id_tracker.clone());
                }
            }
        }
        Ok(())
    }
}

impl SenderAccount {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        sender_id: Address,
        escrow_accounts: Eventual<EscrowAccounts>,
        indexer_allocations: Eventual<HashSet<Address>>,
        escrow_subgraph: &'static SubgraphClient,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoint: String,
    ) -> Self {
        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone(), sender_id);

        Self {
            escrow_accounts,
            indexer_allocations,
            escrow_subgraph,
            escrow_adapter,
            tap_eip712_domain_separator,
            sender_aggregator_endpoint,
            config,
            pgpool,
            sender: sender_id,
        }
    }

    async fn rav_requester_single(
        &self,
        heaviest_allocation: &mut AllocationIdTracker,
    ) -> Result<()> {
        let Some(allocation_id) = heaviest_allocation.get_heaviest_allocation_id() else {
            anyhow::bail!("Error while getting allocation with most unaggregated fees");
        };
        let sender_id = self.sender;
        let allocation =
            ActorRef::<SenderAllocationMsg>::where_is(format!("{sender_id}:{allocation_id}"));

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation with most unaggregated fees");
        };
        // we call and wait for the response so we don't process anymore update
        let result = call!(allocation, SenderAllocationMsg::TriggerRAVRequest)?;

        heaviest_allocation.add_or_update(allocation_id, result.value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::agent::sender_accounts_manager::NewReceiptNotification;
    use alloy_primitives::hex::ToHex;
    use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
    use ethereum_types::U256;
    use indexer_common::allocations::{AllocationStatus, SubgraphDeployment};
    use indexer_common::subgraph_client::DeploymentDetails;
    use serde_json::json;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;
    use tap_aggregator::server::run_server;
    use tap_core::{rav::ReceiptAggregateVoucher, signed_message::EIP712SignedMessage};
    use thegraph::types::DeploymentId;
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
    const VALUE_PER_RECEIPT: u64 = 100;
    const TRIGGER_VALUE: u64 = 500;

    // To help with testing from other modules.

    async fn create_sender_with_allocations(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
    ) -> ActorRef<SenderAccountMessage> {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: TRIGGER_VALUE,
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

        let allocation_id =
            Address::from_str("0xdd975e30aafebb143e54d215db8a3e8fd916a701").unwrap();

        let indexer_allocations = Eventual::from_value(HashSet::from([
            *ALLOCATION_ID_0,
            *ALLOCATION_ID_1,
            *ALLOCATION_ID_2,
        ]));

        let sender = SenderAccount::new(
            config,
            pgpool,
            SENDER.1,
            escrow_accounts_eventual,
            indexer_allocations,
            escrow_subgraph,
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint,
        );

        let (sender, _handle) =
            SenderAccount::spawn(Some(SENDER.1.to_string()), sender, HashSet::new())
                .await
                .unwrap();

        // await for the allocations to be created
        ractor::concurrency::sleep(Duration::from_millis(100)).await;

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

        let allocation = ActorRef::<SenderAllocationMsg>::where_is(format!(
            "{sender}:{allocation_id}",
            sender = SENDER.1,
            allocation_id = ALLOCATION_ID_0.to_string()
        ))
        .unwrap();

        // Check that the sender's unaggregated fees are correct.
        let allocation_tracker = call!(sender, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert_eq!(
            allocation_tracker.get_total_fee(),
            expected_unaggregated_fees
        );

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees are correct.
        assert_eq!(
            allocation_unaggregated_fees.value,
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
        allocation
            .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
            .unwrap();
        ractor::concurrency::sleep(Duration::from_millis(10)).await;

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees have *not* increased.
        assert_eq!(
            allocation_unaggregated_fees.value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        let allocation_tracker = call!(sender, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert_eq!(
            allocation_tracker.get_total_fee(),
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
        allocation
            .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
            .unwrap();
        ractor::concurrency::sleep(Duration::from_millis(10)).await;

        expected_unaggregated_fees += 20;

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees are correct.
        assert_eq!(
            allocation_unaggregated_fees.value,
            expected_unaggregated_fees
        );

        // Check that the sender's unaggregated fees are correct.
        let allocation_tracker = call!(sender, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert_eq!(
            allocation_tracker.get_total_fee(),
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
        allocation
            .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
            .unwrap();

        ractor::concurrency::sleep(Duration::from_millis(10)).await;

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees have *not* increased.
        assert_eq!(
            allocation_unaggregated_fees.value,
            expected_unaggregated_fees
        );

        // Check that the unaggregated fees have *not* increased.
        let allocation_tracker = call!(sender, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert_eq!(
            allocation_tracker.get_total_fee(),
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

        let allocation = ActorRef::<SenderAllocationMsg>::where_is(format!(
            "{sender}:{allocation_id}",
            sender = SENDER.1,
            allocation_id = ALLOCATION_ID_0.to_string()
        ))
        .unwrap();

        // Add receipts to the database and call the `handle_new_receipt_notification` method
        // correspondingly.
        let mut total_value = 0;
        let mut trigger_value = 0;
        for i in 1..=10 {
            // These values should be enough to trigger a RAV request at i == 7 since we set the
            // `rav_request_trigger_value` to 100.
            let value = (i + VALUE_PER_RECEIPT) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, value).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
            let new_receipt_notification = NewReceiptNotification {
                allocation_id: *ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                id: i,
                timestamp_ns: i + 1 ,
                value,
            };
            allocation
                .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
                .unwrap();

            ractor::concurrency::sleep(Duration::from_millis(100)).await;

            total_value += value;
            if total_value >= TRIGGER_VALUE as u128 && trigger_value == 0 {
                trigger_value = total_value;
            }
        }

        ractor::concurrency::sleep(Duration::from_millis(10)).await;

        // Wait for the RAV requester to finish.
        for _ in 0..100 {
            let allocation_tracker =
                call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();
            if allocation_tracker.get_total_fee() < trigger_value {
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

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees have *not* increased.
        assert!(allocation_unaggregated_fees.value <= trigger_value);

        // Check that the sender's unaggregated fees value is reduced.
        let allocation_tracker =
            call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert!(allocation_tracker.get_total_fee() <= trigger_value);

        // Reset the total value and trigger value.
        total_value = allocation_tracker.get_total_fee();
        trigger_value = 0;

        // Add more receipts
        for i in 10..20 {
            let value = (i + VALUE_PER_RECEIPT) as u128;

            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into()).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();

            let new_receipt_notification = NewReceiptNotification {
                allocation_id: *ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                id: i,
                timestamp_ns: i + 1,
                value,
            };
            allocation
                .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
                .unwrap();

            ractor::concurrency::sleep(Duration::from_millis(10)).await;

            total_value += value;
            if total_value >= TRIGGER_VALUE as u128 && trigger_value == 0 {
                trigger_value = total_value;
            }
        }

        // Wait for the RAV requester to finish.
        for _ in 0..100 {
            let allocation_tracker =
                call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();
            if allocation_tracker.get_total_fee() < trigger_value {
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

        let allocation_unaggregated_fees =
            call!(allocation, SenderAllocationMsg::GetUnaggregatedReceipts).unwrap();

        // Check that the allocation's unaggregated fees have *not* increased.
        assert!(allocation_unaggregated_fees.value <= trigger_value);

        // Check that the unaggregated fees value is reduced.
        let allocation_tracker =
            call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();
        assert!(allocation_tracker.get_total_fee() <= trigger_value);

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }

    // #[sqlx::test(migrations = "../migrations")]
    // async fn test_sender_unaggregated_fees(pgpool: PgPool) {
    //     // Create a sender_account.
    //     let sender_account = Arc::new(
    //         create_sender_with_allocations(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await,
    //     );
    //
    //     // Closure that adds a number of receipts to an allocation.
    //     let add_receipts = |allocation_id: Address, iterations: u64| {
    //         let sender_account = sender_account.clone();
    //
    //         async move {
    //             let mut total_value = 0;
    //             for i in 0..iterations {
    //                 let value = (i + 10) as u128;
    //
    //                 let id = sender_account.unaggregated_fees.lock().unwrap().last_id + 1;
    //
    //                 sender_account
    //                     .handle_new_receipt_notification(NewReceiptNotification {
    //                         allocation_id,
    //                         signer_address: SIGNER.1,
    //                         id,
    //                         timestamp_ns: i + 1,
    //                         value,
    //                     })
    //                     .await;
    //
    //                 total_value += value;
    //             }
    //
    //             assert_eq!(
    //                 sender_account
    //                     .allocations
    //                     .lock()
    //                     .unwrap()
    //                     .get(&allocation_id)
    //                     .unwrap()
    //                     .as_active()
    //                     .unwrap()
    //                     .get_unaggregated_fees()
    //                     .value,
    //                 total_value
    //             );
    //
    //             total_value
    //         }
    //     };
    //
    //     // Add receipts to the database for allocation_0
    //     let total_value_0 = add_receipts(*ALLOCATION_ID_0, 9).await;
    //
    //     // Add receipts to the database for allocation_1
    //     let total_value_1 = add_receipts(*ALLOCATION_ID_1, 10).await;
    //
    //     // Add receipts to the database for allocation_2
    //     let total_value_2 = add_receipts(*ALLOCATION_ID_2, 8).await;
    //
    //     // Get the heaviest allocation.
    //     let heaviest_allocation = sender_account.get_heaviest_allocation().unwrap();
    //
    //     // Check that the heaviest allocation is correct.
    //     assert_eq!(heaviest_allocation.get_allocation_id(), *ALLOCATION_ID_1);
    //
    //     // Check that the sender's unaggregated fees value is correct.
    //     assert_eq!(
    //         sender_account.unaggregated_fees.lock().unwrap().value,
    //         total_value_0 + total_value_1 + total_value_2
    //     );
    // }
}
