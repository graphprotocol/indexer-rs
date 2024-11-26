// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use alloy::primitives::Address;
use axum::{body::to_bytes, http::Request};
use axum_extra::headers::Header;
use indexer_config::{BlockchainConfig, GraphNodeConfig, IndexerConfig, NonZeroGRT};
use indexer_monitor::EscrowAccounts;
use indexer_service_rs::{
    service::{ServiceRouter, TapReceipt},
    QueryBody,
};
use reqwest::{Method, StatusCode, Url};
use sqlx::PgPool;
use test_assets::{
    create_signed_receipt, SignedReceiptRequest, INDEXER_ALLOCATIONS, TAP_EIP712_DOMAIN,
};
use tokio::sync::watch;
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[sqlx::test(migrations = "../../migrations")]
async fn full_integration_test(database: PgPool) {
    let http_client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .build()
        .expect("Failed to init HTTP client");

    let allocation = INDEXER_ALLOCATIONS.values().next().unwrap().clone();
    let deployment = allocation.subgraph_deployment.id;

    let mock_server = MockServer::start().await;

    let mock = Mock::given(method("POST"))
        .and(path(format!("/subgraphs/id/{deployment}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                {
                    "data": {
                        "graphNetwork": {
                            "currentEpoch": 960
                        }
                    }
                }
                "#,
            "application/json",
        ));
    mock_server.register(mock).await;

    let (_escrow_tx, escrow_accounts) = watch::channel(EscrowAccounts::new(
        test_assets::ESCROW_ACCOUNTS_BALANCES.clone(),
        test_assets::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.clone(),
    ));
    let (_dispute_tx, dispute_manager) = watch::channel(Address::ZERO);

    let (_allocations_tx, allocations) = watch::channel(test_assets::INDEXER_ALLOCATIONS.clone());

    let graph_node_url = Url::parse(&mock_server.uri()).unwrap();

    let router = ServiceRouter::builder()
        .database(database)
        .domain_separator(TAP_EIP712_DOMAIN.clone())
        .http_client(http_client)
        .graph_node(leak(GraphNodeConfig {
            query_url: graph_node_url.clone(),
            status_url: graph_node_url.clone(),
        }))
        .indexer(leak(IndexerConfig {
            indexer_address: *test_assets::INDEXER_ADDRESS,
            operator_mnemonic: test_assets::INDEXER_MNEMONIC.clone(),
        }))
        .service(leak(indexer_config::ServiceConfig {
            serve_network_subgraph: false,
            serve_escrow_subgraph: false,
            serve_auth_token: None,
            host_and_port: "0.0.0.0:0".parse().unwrap(),
            url_prefix: "/".into(),
            tap: indexer_config::ServiceTapConfig {
                max_receipt_value_grt: NonZeroGRT::new(1000000000000).unwrap(),
            },
            free_query_auth_token: None,
        }))
        .blockchain(leak(BlockchainConfig {
            chain_id: indexer_config::TheGraphChainId::Test,
            receipts_verifier_address: *test_assets::VERIFIER_ADDRESS,
        }))
        .timestamp_buffer_secs(Duration::from_secs(10))
        .escrow_accounts(escrow_accounts)
        .dispute_manager(dispute_manager)
        .allocations(allocations)
        .build();

    let app = router.create_router().await.unwrap();

    let receipt = create_signed_receipt(
        SignedReceiptRequest::builder()
            .allocation_id(allocation.id)
            .value(100)
            .build(),
    )
    .await;

    let query = QueryBody {
        query: "query".into(),
        variables: None,
    };

    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/subgraphs/id/{deployment}"))
        .header(TapReceipt::name(), serde_json::to_string(&receipt).unwrap())
        .body(serde_json::to_string(&query).unwrap())
        .unwrap();

    // with deployment
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let graphql_response = res.into_body();
    let bytes = to_bytes(graphql_response, usize::MAX).await.unwrap();
    let res = String::from_utf8(bytes.into()).unwrap();

    insta::assert_snapshot!(res);
}

fn leak<T>(thing: T) -> &'static T {
    Box::leak(Box::new(thing))
}
