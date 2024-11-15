// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::{
    cli::Cli,
    database, metrics,
    routes::{self, health},
};
use alloy::primitives::Address;
use anyhow::anyhow;
use async_graphql::{EmptySubscription, Schema};
use async_graphql_axum::GraphQL;
use axum::{
    middleware::from_fn,
    routing::{post, post_service},
    Extension, Json, Router,
};
use indexer_config::{Config, DipsConfig};
use thegraph_core::attestation::eip712_domain;

use crate::{
    database::dips::{AgreementStore, InMemoryAgreementStore},
    routes::dips::Price,
};

use axum::{
    extract::{MatchedPath, Request as ExtractRequest},
    routing::get,
    serve, ServiceExt,
};
use clap::Parser;
use indexer_common::{
    address::public_key,
    allocations::monitor::indexer_allocations,
    attestations::{dispute_manager, signer::AttestationSigner, signers::attestation_signers},
    middleware::{
        auth::{self, Bearer, OrExt},
        inject_tap_context::context_middleware,
    },
    monitors::escrow_accounts,
    subgraph_client::{DeploymentDetails, SubgraphClient},
    tap::IndexerTapContext,
};
use reqwest::Method;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use subgraph_service::SubgraphService;
use tap_core::{manager::Manager, receipt::checks::CheckList, tap_eip712_domain};
use tokio::{net::TcpListener, signal, sync::watch::Receiver};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{
    auth::AsyncRequireAuthorizationLayer,
    cors::{self, CorsLayer},
    normalize_path::NormalizePath,
    trace::TraceLayer,
    validate_request::ValidateRequestHeaderLayer,
};
use tracing::{error, info, info_span, warn};
use version_info::IndexerServiceRelease;

mod error;
mod response;
mod subgraph_service;
mod version_info;

pub use error::IndexerServiceError;
pub use response::SubgraphServiceResponse;
pub use subgraph_service::SubgraphServiceState;

pub struct IndexerServiceState {
    pub subgraph_service: SubgraphService,
    pub attestation_signers: Receiver<HashMap<Address, AttestationSigner>>,
}

/// Run the subgraph indexer service
pub async fn run() -> anyhow::Result<()> {
    // Parse command line and environment arguments
    let cli = Cli::parse();

    // Load the json-rpc service configuration, which is a combination of the
    // general configuration options for any indexer service and specific
    // options added for JSON-RPC
    let config = Box::leak(Box::new(
        Config::parse(indexer_config::ConfigPrefix::Service, cli.config.as_ref()).map_err(|e| {
            error!(
                "Invalid configuration file `{}`: {}, if a value is missing you can also use \
                --config to fill the rest of the values",
                cli.config.unwrap_or_default().display(),
                e
            );
            anyhow!(e)
        })?,
    ));

    // Parse basic configurations
    build_info::build_info!(fn build_info);
    let release = IndexerServiceRelease::from(build_info());

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema
    // Note: Typically, you'd call `sqlx::migrate!();` here to sync the models
    // which defaults to files in  "./migrations" to sync the database;
    // however, this can cause conflicts with the migrations run by indexer
    // agent. Hence we leave syncing and migrating entirely to the agent and
    // assume the models are up to date in the service.
    let database =
        database::connect(config.database.clone().get_formated_postgres_url().as_ref()).await;

    // Some of the subgraph service configuration goes into the so-called
    // "state", which will be passed to any request handler, middleware etc.
    // that is involved in serving requests
    let state = Arc::new(SubgraphServiceState {
        database: database.clone(),
        graph_node_client: reqwest::ClientBuilder::new()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to init HTTP client for Graph Node"),
        graph_node_status_url: &config.graph_node.status_url,
        graph_node_query_base_url: &config.graph_node.query_url,
    });

    let agreement_store: Arc<dyn AgreementStore> = Arc::new(InMemoryAgreementStore::default());
    let prices: Vec<Price> = vec![];

    let mut extra_router = Router::new()
        .route(
            "/cost",
            post_service(GraphQL::new(routes::cost::build_schema())),
        )
        .route("/status", post(routes::status))
        .with_state(state.clone());

    if let Some(DipsConfig {
        allowed_payers,
        cancellation_time_tolerance,
    }) = config.dips.as_ref()
    {
        let schema = Schema::build(
            routes::dips::AgreementQuery {},
            routes::dips::AgreementMutation {
                expected_payee: config.indexer.indexer_address,
                allowed_payers: allowed_payers.clone(),
                domain: eip712_domain(
                    // 42161, // arbitrum
                    config.blockchain.chain_id as u64,
                    config.blockchain.receipts_verifier_address,
                ),
                cancel_voucher_time_tolerance: cancellation_time_tolerance
                    .unwrap_or(Duration::from_secs(5)),
            },
            EmptySubscription,
        )
        .data(agreement_store)
        .data(prices)
        .finish();

        extra_router = extra_router.route("/dips", post_service(GraphQL::new(schema)));
    }

    let http_client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to init HTTP client");

    let network_subgraph: &'static SubgraphClient = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            config
                .subgraphs
                .network
                .config
                .deployment_id
                .map(|deployment| {
                    DeploymentDetails::for_graph_node_url(
                        config.graph_node.status_url.clone(),
                        config.graph_node.query_url.clone(),
                        deployment,
                    )
                })
                .transpose()
                .expect(
                    "Failed to parse graph node query endpoint and network subgraph deployment",
                ),
            DeploymentDetails::for_query_url_with_token(
                config.subgraphs.network.config.query_url.as_ref(),
                config.subgraphs.network.config.query_auth_token.clone(),
            )?,
        )
        .await,
    ));

    // Identify the dispute manager for the configured network
    let dispute_manager =
        dispute_manager::dispute_manager(network_subgraph, Duration::from_secs(3600))
            .await
            .expect("Failed to initialize dispute manager");

    // Monitor the indexer's own allocations
    let allocations = indexer_allocations(
        network_subgraph,
        config.indexer.indexer_address,
        config.subgraphs.network.config.syncing_interval_secs,
        config
            .subgraphs
            .network
            .recently_closed_allocation_buffer_secs,
    )
    .await
    .expect("Failed to initialize indexer_allocations watcher");

    // Maintain an up-to-date set of attestation signers, one for each
    // allocation
    let attestation_signers = attestation_signers(
        allocations.clone(),
        config.indexer.operator_mnemonic.clone(),
        config.blockchain.chain_id as u64,
        dispute_manager,
    );

    let escrow_subgraph: &'static SubgraphClient = Box::leak(Box::new(
        SubgraphClient::new(
            http_client,
            config
                .subgraphs
                .escrow
                .config
                .deployment_id
                .map(|deployment| {
                    DeploymentDetails::for_graph_node_url(
                        config.graph_node.status_url.clone(),
                        config.graph_node.query_url.clone(),
                        deployment,
                    )
                })
                .transpose()
                .expect("Failed to parse graph node query endpoint and escrow subgraph deployment"),
            DeploymentDetails::for_query_url_with_token(
                config.subgraphs.escrow.config.query_url.as_ref(),
                config.subgraphs.escrow.config.query_auth_token.clone(),
            )?,
        )
        .await,
    ));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        config.indexer.indexer_address,
        config.subgraphs.escrow.config.syncing_interval_secs,
        true, // Reject thawing signers eagerly
    )
    .await
    .expect("Error creating escrow_accounts channel");

    let domain_separator = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        config.blockchain.receipts_verifier_address,
    );
    let indexer_context = IndexerTapContext::new(database.clone(), domain_separator.clone()).await;
    let timestamp_error_tolerance = config.tap.rav_request.timestamp_buffer_secs;

    let receipt_max_value = config.service.tap.max_receipt_value_grt.get_value();

    let checks = IndexerTapContext::get_checks(
        database,
        allocations,
        escrow_accounts.clone(),
        domain_separator.clone(),
        timestamp_error_tolerance,
        receipt_max_value,
    )
    .await;

    let tap_manager = Box::leak(Box::new(Manager::new(
        domain_separator.clone(),
        indexer_context,
        CheckList::new(checks),
    )));

    let state = Arc::new(IndexerServiceState {
        attestation_signers,
        // tap_manager,
        // escrow_accounts,
        // domain_separator,
        subgraph_service: SubgraphService::new(state),
        // free_query_auth_token: config.service.free_query_auth_token.as_ref(),
    });

    // Rate limits by allowing bursts of 10 requests and requiring 100ms of
    // time between consecutive requests after that, effectively rate
    // limiting to 10 req/s.
    let misc_rate_limiter = GovernorLayer {
        config: Arc::new(
            GovernorConfigBuilder::default()
                .per_millisecond(100)
                .burst_size(10)
                .finish()
                .expect("Failed to set up rate limiting"),
        ),
    };

    let operator_address =
        Json(serde_json::json!({ "publicKey": public_key(&config.indexer.operator_mnemonic)?}));

    let mut misc_routes = Router::new()
        .route("/", get("Service is up and running"))
        .route("/version", get(Json(release)))
        .route("/info", get(operator_address))
        .route("/subgraph/health/:deployment_id", get(health))
        .route_layer(Extension(config.graph_node.clone()))
        .layer(misc_rate_limiter);

    // Rate limits by allowing bursts of 50 requests and requiring 20ms of
    // time between consecutive requests after that, effectively rate
    // limiting to 50 req/s.
    let static_subgraph_rate_limiter = GovernorLayer {
        config: Arc::new(
            GovernorConfigBuilder::default()
                .per_millisecond(20)
                .burst_size(50)
                .finish()
                .expect("Failed to set up rate limiting"),
        ),
    };

    if config.service.serve_network_subgraph {
        if let Some(free_auth_token) = &config.service.serve_auth_token {
            info!("Serving network subgraph at /network");

            let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);
            misc_routes = misc_routes.route(
                "/network",
                post(routes::static_subgraph_request_handler)
                    .route_layer(auth_layer)
                    .route_layer(static_subgraph_rate_limiter.clone())
                    .with_state(network_subgraph),
            );
        } else {
            warn!(
                "`serve_network_subgraph` is enabled but no `serve_auth_token` provided. Disabling it."
            )
        }
    }

    if config.service.serve_escrow_subgraph {
        if let Some(free_auth_token) = &config.service.serve_auth_token {
            info!("Serving escrow subgraph at /escrow");

            let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);
            misc_routes = misc_routes.route(
                "/escrow",
                post(routes::static_subgraph_request_handler)
                    .route_layer(auth_layer)
                    .route_layer(static_subgraph_rate_limiter)
                    .with_state(escrow_subgraph),
            )
        } else {
            warn!(
                "`serve_escrow_subgraph` is enabled but no `serve_auth_token` provided. Disabling it."
            )
        }
    }

    misc_routes = misc_routes.with_state(state.clone());

    let failed_receipt_metric = Box::leak(Box::new(metrics::FAILED_RECEIPT.clone()));
    let tap_auth = auth::tap_receipt_authorize(tap_manager, failed_receipt_metric);

    let mut route = post(routes::request_handler);

    if let Some(free_auth_token) = &config.service.serve_auth_token {
        let free_query = Bearer::new(free_auth_token);
        let result = free_query.or(tap_auth);
        let auth_layer = AsyncRequireAuthorizationLayer::new(result);
        route = route.layer(auth_layer).layer(from_fn(context_middleware));
    } else {
        let auth_layer = AsyncRequireAuthorizationLayer::new(tap_auth);
        route = route.layer(auth_layer).layer(from_fn(context_middleware));
    }
    let data_routes = Router::new()
        .route(
            PathBuf::from(&config.service.url_prefix)
                .join("subgraphs/id/:id")
                .to_str()
                .expect("Failed to set up `/subgraphs/id/:id` route"),
            route,
        )
        .with_state(state.clone());

    let router = NormalizePath::trim_trailing_slash(
        misc_routes
            .merge(data_routes)
            .merge(extra_router)
            .layer(
                CorsLayer::new()
                    .allow_origin(cors::Any)
                    .allow_headers(cors::Any)
                    .allow_methods([Method::OPTIONS, Method::POST, Method::GET]),
            )
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(|req: &ExtractRequest<_>| {
                        let method = req.method();
                        let uri = req.uri();
                        let matched_path = req
                            .extensions()
                            .get::<MatchedPath>()
                            .map(MatchedPath::as_str);

                        info_span!(
                            "http_request",
                            %method,
                            %uri,
                            matched_path,
                        )
                    })
                    // we disable failures here because we doing our own error logging
                    .on_failure(
                        |_error: tower_http::classify::ServerErrorsFailureClass,
                         _latency: Duration,
                         _span: &tracing::Span| {},
                    ),
            )
            .with_state(state),
    );

    metrics::serve_metrics(config.metrics.get_socket_addr());

    info!(
        address = %config.service.host_and_port,
        "Serving requests",
    );
    let listener = TcpListener::bind(&config.service.host_and_port)
        .await
        .expect("Failed to bind to indexer-service port");

    Ok(serve(
        listener,
        ServiceExt::<ExtractRequest>::into_make_service_with_connect_info::<SocketAddr>(router),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?)
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Signal received, starting graceful shutdown");
}
