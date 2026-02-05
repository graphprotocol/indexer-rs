// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, time::Duration};

use axum::{body::to_bytes, extract::ConnectInfo, http::Request, Extension};
use axum_extra::headers::Header;
use base64::prelude::*;
use indexer_config::{
    BlockchainConfig, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    NonZeroGRT, SubgraphConfig,
};
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use indexer_service_rs::{
    service::{ServiceRouter, TapHeader},
    QueryBody,
};
use prost::Message;
use reqwest::{Method, StatusCode, Url};
use tap_aggregator::grpc::v2::SignedReceipt;
use test_assets::{create_signed_receipt_v2, INDEXER_ALLOCATIONS};
use thegraph_core::{alloy::primitives::Address, CollectionId};
use tokio::sync::watch;
use tower::Service;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

struct RouterInputs {
    database: sqlx::PgPool,
    http_client: reqwest::Client,
    graph_node_url: Url,
    subgraph_client: &'static SubgraphClient,
    escrow_subgraph_config: SubgraphConfig,
    network_subgraph_config: SubgraphConfig,
    escrow_accounts_v2: watch::Receiver<EscrowAccounts>,
    allocations:
        watch::Receiver<std::collections::HashMap<Address, indexer_allocation::Allocation>>,
    dispute_manager: watch::Receiver<Address>,
}

fn build_service_router(inputs: RouterInputs) -> ServiceRouter {
    ServiceRouter::builder()
        .database(inputs.database)
        .domain_separator_v2(test_assets::TAP_EIP712_DOMAIN_V2.clone())
        .http_client(inputs.http_client)
        .graph_node(GraphNodeConfig {
            query_url: inputs.graph_node_url.clone(),
            status_url: inputs.graph_node_url,
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
        .blockchain({
            #[allow(deprecated)]
            BlockchainConfig {
                chain_id: indexer_config::TheGraphChainId::Test,
                receipts_verifier_address: test_assets::VERIFIER_ADDRESS,
                receipts_verifier_address_v2: Some(test_assets::VERIFIER_ADDRESS),
                subgraph_service_address: None,
            }
        })
        .timestamp_buffer_secs(Duration::from_secs(10))
        .escrow_subgraph(
            inputs.subgraph_client,
            EscrowSubgraphConfig {
                config: inputs.escrow_subgraph_config,
            },
        )
        .network_subgraph(
            inputs.subgraph_client,
            NetworkSubgraphConfig {
                config: inputs.network_subgraph_config,
                recently_closed_allocation_buffer_secs: Duration::from_secs(0),
                max_data_staleness_mins: 0,
            },
        )
        .escrow_accounts_v2(inputs.escrow_accounts_v2)
        .dispute_manager(inputs.dispute_manager)
        .allocations(inputs.allocations)
        .build()
}

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

    let router = build_service_router(RouterInputs {
        database: database.clone(),
        http_client,
        graph_node_url: graph_node_url.clone(),
        subgraph_client,
        escrow_subgraph_config: escrow_subgraph_config(),
        network_subgraph_config: escrow_subgraph_config(),
        escrow_accounts_v2: escrow_accounts,
        allocations: allocations.clone(),
        dispute_manager: dispute_manager.clone(),
    });

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
    let failing_router = build_service_router(RouterInputs {
        database: database.clone(),
        http_client: reqwest::Client::builder()
            .tcp_nodelay(true)
            .build()
            .unwrap(),
        graph_node_url: failing_graph_node,
        subgraph_client,
        escrow_subgraph_config: escrow_subgraph_config(),
        network_subgraph_config: escrow_subgraph_config(),
        escrow_accounts_v2: escrow_accounts_fail,
        allocations,
        dispute_manager,
    });

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

    let receipt = create_signed_receipt_v2()
        .collection_id(CollectionId::from(allocation.id))
        .value(100)
        .call()
        .await;

    let query = QueryBody {
        query: "query".into(),
        variables: None,
    };

    let protobuf_receipt = SignedReceipt::from(receipt);
    let encoded = protobuf_receipt.encode_to_vec();
    let receipt_b64 = BASE64_STANDARD.encode(encoded);
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/subgraphs/id/{deployment}"))
        .header(TapHeader::name(), receipt_b64)
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
