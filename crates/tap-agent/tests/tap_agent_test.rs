// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use indexer_tap_agent::{
    agent::{
        sender_account::{SenderAccountConfig, SenderAccountMessage},
        sender_accounts_manager::{
            SenderAccountsManager, SenderAccountsManagerArgs, SenderAccountsManagerMessage,
        },
        sender_allocation::SenderAllocationMessage,
    },
    test::{actors::TestableActor, create_received_receipt, get_grpc_url, store_batch_receipts},
};
use ractor::{call, concurrency::JoinHandle, Actor, ActorRef};
use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use test_assets::{
    assert_while_retry, flush_messages, ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2,
    ESCROW_ACCOUNTS_BALANCES, ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS, INDEXER_ADDRESS,
    INDEXER_ALLOCATIONS, TAP_EIP712_DOMAIN, TAP_SENDER, TAP_SIGNER,
};
use thegraph_core::alloy::primitives::Address;
use tokio::sync::{mpsc, watch};
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

pub async fn start_agent(
    pgpool: PgPool,
) -> (
    mpsc::Receiver<SenderAccountsManagerMessage>,
    (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>),
) {
    let escrow_subgraph_mock_server: MockServer = MockServer::start().await;
    escrow_subgraph_mock_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "transactions": [],
                }
            })),
        ))
        .await;

    let network_subgraph_mock_server = MockServer::start().await;

    let (_escrow_tx, escrow_accounts) = watch::channel(EscrowAccounts::new(
        ESCROW_ACCOUNTS_BALANCES.clone(),
        ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.clone(),
    ));
    let (_dispute_tx, _dispute_manager) = watch::channel(Address::ZERO);

    let (_allocations_tx, indexer_allocations1) = watch::channel(INDEXER_ALLOCATIONS.clone());

    let sender_aggregator_endpoints: HashMap<_, _> =
        vec![(TAP_SENDER.1, Url::from_str(&get_grpc_url().await).unwrap())]
            .into_iter()
            .collect();

    let http_client = reqwest::Client::new();

    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            None,
            DeploymentDetails::for_query_url(&network_subgraph_mock_server.uri()).unwrap(),
        )
        .await,
    ));

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            None,
            DeploymentDetails::for_query_url(&escrow_subgraph_mock_server.uri()).unwrap(),
        )
        .await,
    ));

    let config = Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(500),
        max_amount_willing_to_lose_grt: 50,
        trigger_value: 150,
        rav_request_timeout: Duration::from_secs(60),
        rav_request_receipt_limit: 10,
        indexer_address: INDEXER_ADDRESS,
        escrow_polling_interval: Duration::from_secs(10),
        tap_sender_timeout: Duration::from_secs(30),
        trusted_senders: HashSet::new(),
        horizon_enabled: false,
    }));

    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: TAP_EIP712_DOMAIN.clone(),
        pgpool,
        indexer_allocations: indexer_allocations1,
        escrow_accounts_v1: escrow_accounts.clone(),
        escrow_accounts_v2: watch::channel(EscrowAccounts::default()).1,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: None,
    };

    let (sender, receiver) = mpsc::channel(10);
    let actor = TestableActor::new(SenderAccountsManager, sender);
    (receiver, Actor::spawn(None, actor, args).await.unwrap())
}

#[tokio::test]
async fn test_start_tap_agent() {
    let test_db = test_assets::setup_shared_test_db().await;
    let pgpool = test_db.pool;
    let (mut msg_receiver, (_actor_ref, _handle)) = start_agent(pgpool.clone()).await;
    flush_messages(&mut msg_receiver).await;

    // verify if create sender account
    assert_while_retry!(ActorRef::<SenderAccountMessage>::where_is(format!(
        "legacy:{}",
        TAP_SENDER.1
    ))
    .is_none());

    // Add batch receits to the database.
    const AMOUNT_OF_RECEIPTS: u64 = 3000;
    let allocations = [ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2];
    let mut receipts = Vec::with_capacity(AMOUNT_OF_RECEIPTS as usize);
    for i in 0..AMOUNT_OF_RECEIPTS {
        // This would select the 3 defined allocations in order
        let allocation_selected = (i % 3) as usize;
        let receipt = create_received_receipt(
            allocations.get(allocation_selected).unwrap(),
            &TAP_SIGNER.0,
            i,
            i + 1,
            i.into(),
        );
        receipts.push(receipt);
    }
    let res = store_batch_receipts(&pgpool, receipts).await;
    assert!(res.is_ok());

    assert_while_retry!({
        ActorRef::<SenderAllocationMessage>::where_is(format!(
            "{}:{}",
            TAP_SENDER.1, ALLOCATION_ID_0,
        ))
        .is_none()
    });

    let sender_allocation_ref = ActorRef::<SenderAllocationMessage>::where_is(format!(
        "{}:{}",
        TAP_SENDER.1, ALLOCATION_ID_0,
    ))
    .unwrap();

    assert_while_retry!(
        {
            let total_unaggregated_fees = call!(
                sender_allocation_ref,
                SenderAllocationMessage::GetUnaggregatedReceipts
            )
            .unwrap();
            total_unaggregated_fees.value == 0u128
        },
        "Unnagregated fees",
        Duration::from_secs(10),
        Duration::from_millis(50)
    );
}
