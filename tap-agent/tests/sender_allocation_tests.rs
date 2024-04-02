// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use eventuals::Eventual;
use indexer_common::{
    escrow_accounts::EscrowAccounts,
    subgraph_client::{DeploymentDetails, SubgraphClient},
};
use indexer_tap_agent::{
    agent::{
        sender_account::SenderAccountMessage,
        sender_allocation::{SenderAllocation, SenderAllocationArgs, SenderAllocationMessage},
    },
    config,
    tap::escrow_adapter::EscrowAdapter,
};
use ractor::{call, Actor, ActorProcessingErr, ActorRef};
use serde_json::json;
use sqlx::PgPool;
use tap_aggregator::server::run_server;

use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

mod test_utils;

struct MockSenderAccount;

#[async_trait::async_trait]
impl Actor for MockSenderAccount {
    type Msg = SenderAccountMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _allocation_ids: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        Ok(())
    }
}

use test_utils::{
    create_rav, create_received_receipt, store_rav, store_receipt, ALLOCATION_ID_0, INDEXER,
    SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
};

const DUMMY_URL: &str = "http://localhost:1234";

async fn create_sender_allocation(
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
    escrow_subgraph_endpoint: &str,
) -> ActorRef<SenderAllocationMessage> {
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

    let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone(), SENDER.1);

    let (sender_account_ref, _join_handle) = MockSenderAccount::spawn(None, MockSenderAccount, ())
        .await
        .unwrap();

    let args = SenderAllocationArgs {
        config,
        pgpool: pgpool.clone(),
        allocation_id: *ALLOCATION_ID_0,
        sender: SENDER.1,
        escrow_accounts: escrow_accounts_eventual,
        escrow_subgraph,
        escrow_adapter,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        sender_aggregator_endpoint,
        sender_account_ref,
    };

    let (allocation_ref, _join_handle) = SenderAllocation::spawn(None, SenderAllocation, args)
        .await
        .unwrap();

    allocation_ref
}

/// Test that the sender_allocation correctly updates the unaggregated fees from the
/// database when there is no RAV in the database.
///
/// The sender_allocation should consider all receipts found for the allocation and
/// sender.
#[sqlx::test(migrations = "../migrations")]
async fn test_update_unaggregated_fees_no_rav(pgpool: PgPool) {
    // Add receipts to the database.
    for i in 1..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into()).await;
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let sender_allocation =
        create_sender_allocation(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

    // Get total_unaggregated_fees
    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::GetUnaggregatedReceipts
    )
    .unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 45u128);
}

/// Test that the sender_allocation correctly updates the unaggregated fees from the
/// database when there is a RAV in the database as well as receipts which timestamp are lesser
/// and greater than the RAV's timestamp.
///
/// The sender_allocation should only consider receipts with a timestamp greater
/// than the RAV's timestamp.
#[sqlx::test(migrations = "../migrations")]
async fn test_update_unaggregated_fees_with_rav(pgpool: PgPool) {
    // Add the RAV to the database.
    // This RAV has timestamp 4. The sender_allocation should only consider receipts
    // with a timestamp greater than 4.
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10).await;
    store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

    // Add receipts to the database.
    for i in 1..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into()).await;
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let sender_allocation =
        create_sender_allocation(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

    // Get total_unaggregated_fees
    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::GetUnaggregatedReceipts
    )
    .unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 35u128);
}

#[sqlx::test(migrations = "../migrations")]
async fn test_rav_requester_manual(pgpool: PgPool) {
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

    // Add receipts to the database.
    for i in 0..10 {
        let receipt =
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into()).await;
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    // Create a sender_allocation.
    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        "http://".to_owned() + &aggregator_endpoint.to_string(),
        &mock_server.uri(),
    )
    .await;

    // Trigger a RAV request manually.

    // Get total_unaggregated_fees
    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::TriggerRAVRequest
    )
    .unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 0u128);

    // Stop the TAP aggregator server.
    handle.stop().unwrap();
    handle.stopped().await;
}
