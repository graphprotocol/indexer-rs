use std::{collections::HashSet, str::FromStr, sync::atomic::AtomicU32};

use indexer_tap_agent::{
    agent::sender_account::SenderAccountMessage,
    test::{
        create_received_receipt, create_sender_account, store_receipt, TAP_EIP712_DOMAIN_SEPARATOR,
    },
};
use ractor::concurrency::Duration;
use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use tap_aggregator::server::run_server;
use test_assets::{ALLOCATION_ID_0, TAP_SIGNER as SIGNER};
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

pub static PREFIX_ID: AtomicU32 = AtomicU32::new(0);
const TRIGGER_VALUE: u128 = 500;
const RECEIPT_LIMIT: u64 = 10000;

#[sqlx::test(migrations = "../../migrations")]
async fn test_rav_marked_as_last_closing_sender_allocation(pgpool: PgPool) {
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

    let mock_server = MockServer::start().await;

    let no_allocations_closed_guard = mock_server
        .register_as_scoped(
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
                    "allocations": []
                }
                }))),
        )
        .await;

    let mock_escrow_subgraph_server: MockServer = MockServer::start().await;
    mock_escrow_subgraph_server
        .register(
            Mock::given(method("POST"))
                // .and(body_string_contains("TapTransactions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                    "transactions": [],
                }
                }))),
        )
        .await;

    let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, TRIGGER_VALUE - 100);
    store_receipt(&pgpool, receipt.signed_receipt())
        .await
        .unwrap();

    let (sender_account, notify, _, _) = create_sender_account()
        .pgpool(pgpool.clone())
        .initial_allocation(HashSet::new())
        .rav_request_trigger_value(TRIGGER_VALUE)
        .max_amount_willing_to_lose_grt(TRIGGER_VALUE + 1000)
        .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
        .network_subgraph_endpoint(&mock_server.uri())
        .rav_request_receipt_limit(RECEIPT_LIMIT)
        .aggregator_endpoint(
            Url::from_str(&("http://".to_owned() + &aggregator_endpoint.to_string()))
                .expect("This shouldnt fail"),
        )
        .call()
        .await;

    // we expect it to create a sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![ALLOCATION_ID_0].into_iter().collect(),
        ))
        .unwrap();
    notify.notified().await;

    drop(no_allocations_closed_guard);
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
                SELECT * FROM scalar_tap_ravs WHERE last = true;
            "#,
    )
    .fetch_all(&pgpool)
    .await
    .expect("Should not fail to fetch from scalar_tap_ravs");
    assert!(!rav_marked_as_last.is_empty());
    // Stop the TAP aggregator server.
    handle.abort();
    // handle.stop().unwrap();
    // handle.stopped().await;
}
