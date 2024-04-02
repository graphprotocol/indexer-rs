// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::hex::ToHex;
use alloy_primitives::Address;
use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::subgraph_client::{DeploymentDetails, SubgraphClient};
use indexer_tap_agent::agent::sender_account::{
    SenderAccount, SenderAccountArgs, SenderAccountMessage,
};
use indexer_tap_agent::agent::sender_accounts_manager::NewReceiptNotification;
use indexer_tap_agent::agent::sender_allocation::SenderAllocationMessage;
use indexer_tap_agent::agent::unaggregated_receipts::UnaggregatedReceipts;
use indexer_tap_agent::config;
use ractor::{call, cast, Actor, ActorRef};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;
use std::{collections::HashMap, sync::atomic::AtomicU32};
use tap_aggregator::server::run_server;
use tap_core::{rav::ReceiptAggregateVoucher, signed_message::EIP712SignedMessage};
use tokio::task::JoinHandle;
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

use test_utils::{
    create_received_receipt, store_receipt, ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2,
    INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
};

mod test_utils;

const DUMMY_URL: &str = "http://localhost:1234";
const VALUE_PER_RECEIPT: u64 = 100;
const TRIGGER_VALUE: u64 = 500;
static PREFIX_ID: AtomicU32 = AtomicU32::new(0);

// To help with testing from other modules.

async fn create_sender_with_allocations(
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
    escrow_subgraph_endpoint: &str,
) -> (ActorRef<SenderAccountMessage>, JoinHandle<()>, String) {
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

    let indexer_allocations = Eventual::from_value(HashSet::from([
        *ALLOCATION_ID_0,
        *ALLOCATION_ID_1,
        *ALLOCATION_ID_2,
    ]));

    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );

    let args = SenderAccountArgs {
        config,
        pgpool,
        sender_id: SENDER.1,
        escrow_accounts: escrow_accounts_eventual,
        indexer_allocations,
        escrow_subgraph,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        sender_aggregator_endpoint,
        allocation_ids: HashSet::new(),
        prefix: Some(prefix.clone()),
    };

    let (sender, handle) = SenderAccount::spawn(Some(prefix.clone()), SenderAccount, args)
        .await
        .unwrap();

    // await for the allocations to be created
    ractor::concurrency::sleep(Duration::from_millis(100)).await;

    (sender, handle, prefix)
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
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into()).await;
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
        expected_unaggregated_fees += u128::from(i);
    }

    let (sender, handle, prefix) =
        create_sender_with_allocations(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

    let allocation = ActorRef::<SenderAllocationMessage>::where_is(format!(
        "{prefix}:{sender}:{allocation_id}",
        sender = SENDER.1,
        allocation_id = *ALLOCATION_ID_0
    ))
    .unwrap();

    // Check that the sender's unaggregated fees are correct.
    let allocation_tracker = call!(sender, SenderAccountMessage::GetAllocationTracker).unwrap();
    assert_eq!(
        allocation_tracker.get_total_fee(),
        expected_unaggregated_fees
    );

    let allocation_unaggregated_fees =
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

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
        .cast(SenderAllocationMessage::NewReceipt(
            new_receipt_notification,
        ))
        .unwrap();
    ractor::concurrency::sleep(Duration::from_millis(10)).await;

    let allocation_unaggregated_fees =
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

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
        .cast(SenderAllocationMessage::NewReceipt(
            new_receipt_notification,
        ))
        .unwrap();
    ractor::concurrency::sleep(Duration::from_millis(10)).await;

    expected_unaggregated_fees += 20;

    let allocation_unaggregated_fees =
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

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
        .cast(SenderAllocationMessage::NewReceipt(
            new_receipt_notification,
        ))
        .unwrap();

    ractor::concurrency::sleep(Duration::from_millis(10)).await;

    let allocation_unaggregated_fees =
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

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

    sender.stop(None);
    handle.await.unwrap();
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
    let (sender_account, sender_handle, prefix) = create_sender_with_allocations(
        pgpool.clone(),
        "http://".to_owned() + &aggregator_endpoint.to_string(),
        &mock_server.uri(),
    )
    .await;

    let allocation = ActorRef::<SenderAllocationMessage>::where_is(format!(
        "{prefix}:{sender}:{allocation_id}",
        sender = SENDER.1,
        allocation_id = *ALLOCATION_ID_0
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

        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, value).await;
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
            .cast(SenderAllocationMessage::NewReceipt(
                new_receipt_notification,
            ))
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
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

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
            .cast(SenderAllocationMessage::NewReceipt(
                new_receipt_notification,
            ))
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
        call!(allocation, SenderAllocationMessage::GetUnaggregatedReceipts).unwrap();

    // Check that the allocation's unaggregated fees have *not* increased.
    assert!(allocation_unaggregated_fees.value <= trigger_value);

    // Check that the unaggregated fees value is reduced.
    let allocation_tracker =
        call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();
    assert!(allocation_tracker.get_total_fee() <= trigger_value);

    // Stop the TAP aggregator server.
    handle.stop().unwrap();
    handle.stopped().await;

    sender_account.stop(None);
    sender_handle.await.unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn test_sender_unaggregated_fees(pgpool: PgPool) {
    // Create a sender_account.
    let (sender_account, handle, _prefix) =
        create_sender_with_allocations(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Closure that adds a number of receipts to an allocation.

    let update_receipt_fees =
        |sender_account: &ActorRef<SenderAccountMessage>, allocation_id: Address, value: u128| {
            cast!(
                sender_account,
                SenderAccountMessage::UpdateReceiptFees(
                    allocation_id,
                    UnaggregatedReceipts {
                        value,
                        ..Default::default()
                    },
                )
            )
            .unwrap();

            value
        };

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Add receipts to the database for allocation_0
    let total_value_0 = update_receipt_fees(&sender_account, *ALLOCATION_ID_0, 90);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Add receipts to the database for allocation_1
    let total_value_1 = update_receipt_fees(&sender_account, *ALLOCATION_ID_1, 100);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Add receipts to the database for allocation_2
    let total_value_2 = update_receipt_fees(&sender_account, *ALLOCATION_ID_2, 80);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let allocation_tracker =
        call!(sender_account, SenderAccountMessage::GetAllocationTracker).unwrap();

    // Get the heaviest allocation.
    let heaviest_allocation = allocation_tracker.get_heaviest_allocation_id().unwrap();

    // Check that the heaviest allocation is correct.
    assert_eq!(heaviest_allocation, *ALLOCATION_ID_1);

    // Check that the sender's unaggregated fees value is correct.
    assert_eq!(
        allocation_tracker.get_total_fee(),
        total_value_0 + total_value_1 + total_value_2
    );

    sender_account.stop(None);
    handle.await.unwrap();
}
