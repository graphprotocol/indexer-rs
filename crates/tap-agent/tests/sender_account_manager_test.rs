// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, str::FromStr};

use indexer_tap_agent::{
    agent::{
        sender_account::SenderAccountMessage, sender_accounts_manager::SenderAccountsManagerMessage,
    },
    test::{
        create_received_receipt, create_sender_accounts_manager, store_receipt, ALLOCATION_ID_0,
        TAP_EIP712_DOMAIN_SEPARATOR,
    },
};
use ractor::ActorRef;
use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use tap_aggregator::server::run_server;
use test_assets::{flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

const TRIGGER_VALUE: u128 = 100;

#[sqlx::test(migrations = "../../migrations")]
async fn test_account_manger_to_sender_allocation_closing(pgpool: PgPool) {
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

    let aggregator_endpoint = Some(
        Url::from_str(&("http://".to_owned() + &aggregator_endpoint.to_string()))
            .expect("This shouldn't fail"),
    );

    let mock_network_subgraph_server: MockServer = MockServer::start().await;

    mock_network_subgraph_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "meta": {
                            "block": {
                                "number": 1,
                                "hash": "hash",
                                "timestamp": 1
                            }
                        },
                        "allocations": [
                            {"id": *ALLOCATION_ID_0 }
                        ]
                    }
                }))),
        )
        .await;

    let mock_escrow_subgraph_server: MockServer = MockServer::start().await;
    mock_escrow_subgraph_server
        .register(
            Mock::given(method("POST"))
                //.and(body_string_contains("TapTransactions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "transactions": [],
                    }
                }))),
        )
        .await;

    let (prefix, notify, (actor, join_handle)) = create_sender_accounts_manager()
        .pgpool(pgpool.clone())
        .aggregator_endpoint(aggregator_endpoint.expect("should be valid"))
        .network_subgraph(&mock_network_subgraph_server.uri())
        .escrow_subgraph(&mock_escrow_subgraph_server.uri())
        .call()
        .await;

    actor
        .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
            vec![SENDER.1].into_iter().collect(),
        ))
        .unwrap();
    flush_messages(&notify).await;

    // // verify if create sender account
    let actor_ref =
        ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix.clone(), SENDER.1));
    assert!(actor_ref.is_some());

    let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, TRIGGER_VALUE - 10);
    store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .unwrap();

    // Create a sender allocation inside sender account

    // // we expect it to create a sender allocation
    actor_ref
        .clone()
        .unwrap()
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![ALLOCATION_ID_0].into_iter().collect(),
        ))
        .unwrap();
    flush_messages(&notify).await;

    // try to delete sender allocation_id
    actor_ref
        .clone()
        .unwrap()
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    actor_ref.unwrap().stop_children_and_wait(None, None).await;

    // this calls and closes acounts manager sender accounts
    actor
        .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
            HashSet::new(),
        ))
        .unwrap();

    flush_messages(&notify).await;
    // verify if it gets removed
    let actor_ref = ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER.1));
    assert!(actor_ref.is_none());

    //verify the rav is marked as last
    let rav_marked_as_last = sqlx::query!(
        r#"
            SELECT * FROM scalar_tap_ravs WHERE last = true;
        "#,
    )
    .fetch_all(&pgpool)
    .await
    .expect("Should not fail to fetch from scalar_tap_ravs");

    assert!(!rav_marked_as_last.is_empty());

    // safely stop the manager
    actor.stop_and_wait(None, None).await.unwrap();
    join_handle.await.unwrap();

    // Stop the TAP aggregator server.
    handle.abort();
    // handle.stop().unwrap();
    // handle.stopped().await;
}
