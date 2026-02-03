// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, time::Duration};

use axum::{body::to_bytes, extract::ConnectInfo, http::Request, Extension};
use axum_extra::headers::Header;
use indexer_config::{
    BlockchainConfig, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    NonZeroGRT, SubgraphConfig,
};
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use indexer_service_rs::{
    service::{ServiceRouter, TapHeader},
    QueryBody,
};
use reqwest::{Method, StatusCode, Url};
use test_assets::{
    create_signed_receipt, SignedReceiptRequest, INDEXER_ALLOCATIONS, TAP_EIP712_DOMAIN,
};
use thegraph_core::alloy::primitives::Address;
use tokio::sync::watch;
use tower::Service;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn full_integration_test() {
    let database = test_assets::setup_shared_test_db().await.pool;
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

    // Mock escrow subgraph (v1) and network subgraph (v2) redemption queries.
    mock_server
        .register(Mock::given(method("POST")).and(path("/")).respond_with(
            ResponseTemplate::new(200).set_body_raw(
                r#"
                        {
                            "data": {
                                "transactions": [],
                                "paymentsEscrowTransactions": []
                            }
                        }
                        "#,
                "application/json",
            ),
        ))
        .await;

    let (_escrow_tx, escrow_accounts) = watch::channel(EscrowAccounts::new(
        test_assets::ESCROW_ACCOUNTS_BALANCES.clone(),
        test_assets::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.clone(),
    ));
    let (_dispute_tx, dispute_manager) = watch::channel(Address::ZERO);

    let (_allocations_tx, allocations) = watch::channel(test_assets::INDEXER_ALLOCATIONS.clone());

    let graph_node_url = Url::parse(&mock_server.uri()).unwrap();

    let subgraph_client = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
        )
        .await,
    ));

    let subgraph_query_url = Url::parse(&mock_server.uri()).unwrap();

    let escrow_subgraph_config = || SubgraphConfig {
        query_url: subgraph_query_url.clone(),
        query_auth_token: None,
        deployment_id: None,
        syncing_interval_secs: Duration::from_secs(1),
    };

    let router = ServiceRouter::builder()
        .database(database.clone())
        .domain_separator(TAP_EIP712_DOMAIN.clone())
        .domain_separator_v2(test_assets::TAP_EIP712_DOMAIN_V2.clone())
        .http_client(http_client)
        .graph_node(GraphNodeConfig {
            query_url: graph_node_url.clone(),
            status_url: graph_node_url.clone(),
        })
        .indexer(IndexerConfig {
            indexer_address: test_assets::INDEXER_ADDRESS,
            operator_mnemonic: Some(test_assets::INDEXER_MNEMONIC.clone()),
            operator_mnemonics: None,
        })
        .service(indexer_config::ServiceConfig {
            serve_network_subgraph: false,
            serve_escrow_subgraph: false,
            serve_auth_token: None,
            host_and_port: "0.0.0.0:0".parse().unwrap(),
            url_prefix: "/".into(),
            tap: indexer_config::ServiceTapConfig {
                max_receipt_value_grt: NonZeroGRT::new(1000000000000).unwrap(),
            },
            free_query_auth_token: None,
            ipfs_url: "http://localhost:5001".parse().unwrap(),
            max_cost_model_batch_size: 200,
            max_request_body_size: 2 * 1024 * 1024,
        })
        .blockchain(BlockchainConfig {
            chain_id: indexer_config::TheGraphChainId::Test,
            receipts_verifier_address: test_assets::VERIFIER_ADDRESS,
            receipts_verifier_address_v2: None,
            subgraph_service_address: None,
        })
        .timestamp_buffer_secs(Duration::from_secs(10))
        .escrow_subgraph(
            subgraph_client,
            EscrowSubgraphConfig {
                config: escrow_subgraph_config(),
            },
        )
        .network_subgraph(
            subgraph_client,
            NetworkSubgraphConfig {
                config: escrow_subgraph_config(),
                recently_closed_allocation_buffer_secs: Duration::from_secs(0),
                max_data_staleness_mins: 0,
            },
        )
        .escrow_accounts_v1(escrow_accounts.clone())
        .escrow_accounts_v2(escrow_accounts)
        .dispute_manager(dispute_manager.clone())
        .allocations(allocations.clone())
        .build();

    let socket_info = Extension(ConnectInfo(SocketAddr::from(([0, 0, 0, 0], 1337))));
    let mut app = router.create_router().await.unwrap().layer(socket_info);

    let res = app
        .call(Request::get("/").body(String::new()).unwrap())
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let graphql_response = res.into_body();
    let bytes = to_bytes(graphql_response, usize::MAX).await.unwrap();
    let res = String::from_utf8(bytes.into()).unwrap();
    insta::assert_snapshot!(res);

    let res = app
        .call(Request::get("/healthz").body(String::new()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let healthz: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(healthz["status"], "healthy");

    let fail_server = MockServer::start().await;
    fail_server
        .register(Mock::given(method("POST")).respond_with(ResponseTemplate::new(500)))
        .await;
    let failing_graph_node = Url::parse(&fail_server.uri()).unwrap();
    let (_escrow_tx_fail, escrow_accounts_fail) = watch::channel(EscrowAccounts::new(
        test_assets::ESCROW_ACCOUNTS_BALANCES.clone(),
        test_assets::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.clone(),
    ));
    let failing_router = ServiceRouter::builder()
        .database(database.clone())
        .domain_separator(TAP_EIP712_DOMAIN.clone())
        .domain_separator_v2(test_assets::TAP_EIP712_DOMAIN_V2.clone())
        .http_client(
            reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .unwrap(),
        )
        .graph_node(GraphNodeConfig {
            query_url: failing_graph_node.clone(),
            status_url: failing_graph_node,
        })
        .indexer(IndexerConfig {
            indexer_address: test_assets::INDEXER_ADDRESS,
            operator_mnemonic: Some(test_assets::INDEXER_MNEMONIC.clone()),
            operator_mnemonics: None,
        })
        .service(indexer_config::ServiceConfig {
            serve_network_subgraph: false,
            serve_escrow_subgraph: false,
            serve_auth_token: None,
            host_and_port: "0.0.0.0:0".parse().unwrap(),
            url_prefix: "/".into(),
            tap: indexer_config::ServiceTapConfig {
                max_receipt_value_grt: NonZeroGRT::new(1000000000000).unwrap(),
            },
            free_query_auth_token: None,
            ipfs_url: "http://localhost:5001".parse().unwrap(),
            max_cost_model_batch_size: 200,
            max_request_body_size: 2 * 1024 * 1024,
        })
        .blockchain(BlockchainConfig {
            chain_id: indexer_config::TheGraphChainId::Test,
            receipts_verifier_address: test_assets::VERIFIER_ADDRESS,
            receipts_verifier_address_v2: None,
            subgraph_service_address: None,
        })
        .timestamp_buffer_secs(Duration::from_secs(10))
        .escrow_subgraph(
            subgraph_client,
            EscrowSubgraphConfig {
                config: escrow_subgraph_config(),
            },
        )
        .network_subgraph(
            subgraph_client,
            NetworkSubgraphConfig {
                config: escrow_subgraph_config(),
                recently_closed_allocation_buffer_secs: Duration::from_secs(0),
                max_data_staleness_mins: 0,
            },
        )
        .escrow_accounts_v1(escrow_accounts_fail.clone())
        .escrow_accounts_v2(escrow_accounts_fail)
        .dispute_manager(dispute_manager)
        .allocations(allocations)
        .build();

    let mut failing_app = failing_router
        .create_router()
        .await
        .unwrap()
        .layer(socket_info);
    let res = failing_app
        .call(Request::get("/healthz").body(String::new()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

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
        .header(TapHeader::name(), serde_json::to_string(&receipt).unwrap())
        .body(serde_json::to_string(&query).unwrap())
        .unwrap();

    // with deployment
    let res = app.call(request).await.unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let graphql_response = res.into_body();
    let bytes = to_bytes(graphql_response, usize::MAX).await.unwrap();
    let res = String::from_utf8(bytes.into()).unwrap();

    insta::assert_snapshot!(res);

    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/subgraphs/id/{deployment}"))
        .body(serde_json::to_string(&query).unwrap())
        .unwrap();

    // without tap receipt
    let res = app.call(request).await.unwrap();

    assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);

    let graphql_response = res.into_body();
    let bytes = to_bytes(graphql_response, usize::MAX).await.unwrap();
    let res = String::from_utf8(bytes.into()).unwrap();

    insta::assert_snapshot!(res);
}
