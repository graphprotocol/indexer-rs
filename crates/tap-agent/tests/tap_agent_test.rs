// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    str::FromStr,
    sync::{atomic::AtomicU32, Arc},
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
    test::{actors::TestableActor, create_received_receipt, store_batch_receipts},
};
use ractor::{call, concurrency::JoinHandle, Actor, ActorRef};
use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use tap_aggregator::server::run_server;
use tap_core::receipt::{state::Checking, ReceiptWithState};
use test_assets::{
    assert_while_retry, flush_messages, ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2,
    ESCROW_ACCOUNTS_BALANCES, ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS, INDEXER_ADDRESS,
    INDEXER_ALLOCATIONS, TAP_EIP712_DOMAIN, TAP_SENDER, TAP_SIGNER,
};
use thegraph_core::alloy::primitives::Address;
use tokio::sync::{watch, Notify};
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

async fn mock_escrow_subgraph_empty_response() -> MockServer {
    let mock_ecrow_subgraph_server: MockServer = MockServer::start().await;
    let _mock_ecrow_subgraph = mock_ecrow_subgraph_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "transactions": [],
                }
            })),
        ))
        .await;
    mock_ecrow_subgraph_server
}
pub async fn start_agent(
    pgpool: PgPool,
) -> (
    String,
    Arc<Notify>,
    (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>),
    JoinHandle<()>,
) {
    let escrow_subgraph_mock_server = mock_escrow_subgraph_empty_response().await;

    let network_subgraph_mock_server = MockServer::start().await;
    // Start a TAP aggregator server.
    let (handle_aggregator, aggregator_endpoint) = run_server(
        0,
        TAP_SIGNER.0.clone(),
        vec![TAP_SIGNER.1].into_iter().collect(),
        TAP_EIP712_DOMAIN.clone(),
        100 * 1024,
        100 * 1024,
        1,
    )
    .await
    .unwrap();

    let (_escrow_tx, escrow_accounts) = watch::channel(EscrowAccounts::new(
        ESCROW_ACCOUNTS_BALANCES.clone(),
        ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.clone(),
    ));
    let (_dispute_tx, _dispute_manager) = watch::channel(Address::ZERO);

    let (_allocations_tx, indexer_allocations1) = watch::channel(INDEXER_ALLOCATIONS.clone());

    let sender_aggregator_endpoints: HashMap<_, _> = vec![(
        TAP_SENDER.1,
        Url::from_str(&format!("http://{}", aggregator_endpoint)).unwrap(),
    )]
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
    }));
    pub static PREFIX_ID: AtomicU32 = AtomicU32::new(0);
    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: TAP_EIP712_DOMAIN.clone(),
        pgpool,
        indexer_allocations: indexer_allocations1,
        escrow_accounts,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: sender_aggregator_endpoints.clone(),
        prefix: Some(prefix.clone()),
    };

    // let actorr = SenderAccountsManager::spawn(None, SenderAccountsManager, args)
    //     .await
    //     .expect("Failed to start sender accounts manager actor.");
    // actorr

    let actor = TestableActor::new(SenderAccountsManager);
    let notify = actor.notify.clone();
    (
        prefix,
        notify,
        Actor::spawn(None, actor, args).await.unwrap(),
        handle_aggregator,
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_start_tap_agent(pgpool: PgPool) {
    let (prefix, notify, (_actor_ref, _handle), _aggregator_handle) =
        start_agent(pgpool.clone()).await;
    flush_messages(&notify).await;

    // verify if create sender account
    let actor_ref =
        ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix.clone(), TAP_SENDER.1));

    assert!(actor_ref.is_some());

    // Add batch receits to the database.
    const AMOUNT_OF_RECEIPTS: u64 = 3000;
    let allocations = [ALLOCATION_ID_0, ALLOCATION_ID_1, ALLOCATION_ID_2];
    let mut receipts: Vec<ReceiptWithState<Checking>> =
        Vec::with_capacity(AMOUNT_OF_RECEIPTS as usize);
    for i in 0..AMOUNT_OF_RECEIPTS {
        // This would select the 3 defined allocations in order
        let allocation_selected = (i % 3) as usize;
        let receipt = create_received_receipt(
            &allocations.get(allocation_selected).unwrap(),
            &TAP_SIGNER.0,
            i,
            i + 1,
            i.into(),
        );
        receipts.push(receipt);
    }
    let res = store_batch_receipts(&pgpool, receipts).await;
    assert!(res.is_ok());
    let sender_allocation_ref = ActorRef::<SenderAllocationMessage>::where_is(format!(
        "{}:{}:{}",
        prefix.clone(),
        TAP_SENDER.1,
        ALLOCATION_ID_0,
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
