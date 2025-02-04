// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use indexer_tap_agent::{
    agent::{
        sender_account::SenderAccountMessage,
        sender_accounts_manager::{AllocationId, SenderAccountsManagerMessage},
        sender_allocation::SenderAllocationMessage,
    },
    test::{
        create_received_receipt, create_sender_accounts_manager, store_receipt, ALLOCATION_ID_0,
    },
};
use ractor::ActorRef;
use serde_json::json;
use sqlx::PgPool;
use test_assets::{flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

const TRIGGER_VALUE: u128 = 100;

// This test should ensure the full flow starting from
// sender account manager layer to work, up to closing an allocation
#[sqlx::test(migrations = "../../migrations")]
async fn sender_account_manager_layer_test(pgpool: PgPool) {
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
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "transactions": [],
                }
            })),
        ))
        .await;

    let (prefix, notify, (actor, join_handle)) = create_sender_accounts_manager()
        .pgpool(pgpool.clone())
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

    // verify if create sender account
    let actor_ref =
        ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix.clone(), SENDER.1));
    assert!(actor_ref.is_some());

    let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, TRIGGER_VALUE - 10);
    store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .unwrap();

    // we expect it to create a sender allocation
    actor_ref
        .clone()
        .unwrap()
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![AllocationId::Legacy(ALLOCATION_ID_0)]
                .into_iter()
                .collect(),
        ))
        .unwrap();
    flush_messages(&notify).await;

    // try to delete sender allocation_id
    actor_ref
        .clone()
        .unwrap()
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();
    flush_messages(&notify).await;

    assert!(ActorRef::<SenderAllocationMessage>::where_is(format!(
        "{}:{}:{}",
        prefix, SENDER.1, ALLOCATION_ID_0,
    ))
    .is_none());

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
}
