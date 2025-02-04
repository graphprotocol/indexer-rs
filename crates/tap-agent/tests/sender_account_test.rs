// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use indexer_tap_agent::{
    agent::{sender_account::SenderAccountMessage, sender_accounts_manager::AllocationId},
    test::{create_received_receipt, create_sender_account, store_receipt},
};
use ractor::concurrency::Duration;
use serde_json::json;
use sqlx::PgPool;
use test_assets::{ALLOCATION_ID_0, TAP_SIGNER as SIGNER};
use thegraph_core::alloy::hex::ToHexExt;
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

const TRIGGER_VALUE: u128 = 500;

// This test should ensure the full flow starting from
// sender account layer to work, up to closing an allocation
#[sqlx::test(migrations = "../../migrations")]
async fn sender_account_layer_test(pgpool: PgPool) {
    let mock_server = MockServer::start().await;
    let mock_escrow_subgraph_server: MockServer = MockServer::start().await;
    mock_escrow_subgraph_server
        .register(Mock::given(method("POST")).respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": {
                "transactions": [],
            }
            })),
        ))
        .await;

    let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, TRIGGER_VALUE - 100);
    store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .unwrap();

    let (sender_account, notify, _, _) = create_sender_account()
        .pgpool(pgpool.clone())
        .max_amount_willing_to_lose_grt(TRIGGER_VALUE + 1000)
        .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
        .network_subgraph_endpoint(&mock_server.uri())
        .call()
        .await;

    // we expect it to create a sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![AllocationId::Legacy(ALLOCATION_ID_0)]
                .into_iter()
                .collect(),
        ))
        .unwrap();
    notify.notified().await;

    mock_server
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

    // try to delete sender allocation_id
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    sender_account
        .stop_children_and_wait(None, Some(Duration::from_secs(10)))
        .await;

    let rav_marked_as_last = sqlx::query!(
        r#"
                SELECT * FROM scalar_tap_ravs WHERE last = true AND allocation_id = $1;
            "#,
        ALLOCATION_ID_0.encode_hex()
    )
    .fetch_all(&pgpool)
    .await
    .expect("Should not fail to fetch from scalar_tap_ravs");
    assert!(!rav_marked_as_last.is_empty());
}
