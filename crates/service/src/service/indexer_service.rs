// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow;
use axum::{
    extract::{MatchedPath, Request as ExtractRequest},
    http::{Method, Request},
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post},
    serve, Json, Router, ServiceExt,
};
use build_info::BuildInfo;
use indexer_monitor::{
    attestation_signers, deployment_to_allocation, dispute_manager, escrow_accounts,
    indexer_allocations, DeploymentDetails, SubgraphClient,
};
use prometheus::TextEncoder;
use reqwest::StatusCode;
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tap_core::{manager::Manager, receipt::checks::CheckList, tap_eip712_domain};
use tokio::{net::TcpListener, signal};
use tower::ServiceBuilder;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{
    auth::AsyncRequireAuthorizationLayer,
    cors::{self, CorsLayer},
    normalize_path::NormalizePath,
    trace::TraceLayer,
    validate_request::ValidateRequestHeaderLayer,
};
use tracing::{error, info, info_span, warn};

use crate::{
    metrics::{FAILED_RECEIPT, HANDLER_FAILURE, HANDLER_HISTOGRAM},
    middleware::{
        allocation_middleware, attestation_middleware,
        auth::{self, Bearer, OrExt},
        context_middleware, deployment_middleware, labels_middleware, receipt_middleware,
        sender_middleware, signer_middleware, AllocationState, AttestationState,
        PrometheusMetricsMiddlewareLayer, SenderState,
    },
    routes::{health, request_handler, static_subgraph_request_handler},
    tap::IndexerTapContext,
    wallet::public_key,
};
use indexer_config::Config;

use super::SubgraphServiceState;

#[derive(Clone, Serialize)]
pub struct IndexerServiceRelease {
    version: String,
    dependencies: HashMap<String, String>,
}

impl From<&BuildInfo> for IndexerServiceRelease {
    fn from(value: &BuildInfo) -> Self {
        Self {
            version: value.crate_info.version.to_string(),
            dependencies: HashMap::from_iter(
                value
                    .crate_info
                    .dependencies
                    .iter()
                    .map(|d| (d.name.clone(), d.version.to_string())),
            ),
        }
    }
}

pub struct IndexerServiceOptions {
    pub subgraph_state: SubgraphServiceState,
    pub config: &'static Config,
    pub release: IndexerServiceRelease,
    pub url_namespace: &'static str,
    pub extra_routes: Router<SubgraphServiceState>,
}

const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(30);
const DATABASE_MAX_CONNECTIONS: u32 = 50;

const MISC_BURST_SIZE: u32 = 10;
const MISC_BURST_PER_MILLISECOND: u64 = 100;

const STATIC_BURST_SIZE: u32 = 50;
const STATIC_BURST_PER_MILLISECOND: u64 = 20;

const DISPUTE_MANAGER_INTERVAL: Duration = Duration::from_secs(3600);

pub async fn run(options: IndexerServiceOptions) -> Result<(), anyhow::Error> {
    let http_client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .timeout(HTTP_CLIENT_TIMEOUT)
        .build()
        .expect("Failed to init HTTP client");

    let network_subgraph: &'static SubgraphClient = Box::leak(Box::new(
        SubgraphClient::new(
            http_client.clone(),
            options
                .config
                .subgraphs
                .network
                .config
                .deployment_id
                .map(|deployment| {
                    DeploymentDetails::for_graph_node_url(
                        options.config.graph_node.status_url.clone(),
                        options.config.graph_node.query_url.clone(),
                        deployment,
                    )
                })
                .transpose()
                .expect(
                    "Failed to parse graph node query endpoint and network subgraph deployment",
                ),
            DeploymentDetails::for_query_url_with_token(
                options.config.subgraphs.network.config.query_url.as_ref(),
                options
                    .config
                    .subgraphs
                    .network
                    .config
                    .query_auth_token
                    .clone(),
            )?,
        )
        .await,
    ));

    // Identify the dispute manager for the configured network
    let dispute_manager = dispute_manager(network_subgraph, DISPUTE_MANAGER_INTERVAL)
        .await
        .expect("Failed to initialize dispute manager");

    // Monitor the indexer's own allocations
    let allocations = indexer_allocations(
        network_subgraph,
        options.config.indexer.indexer_address,
        options
            .config
            .subgraphs
            .network
            .config
            .syncing_interval_secs,
        options
            .config
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
        options.config.indexer.operator_mnemonic.clone(),
        options.config.blockchain.chain_id as u64,
        dispute_manager,
    );

    let escrow_subgraph: &'static SubgraphClient = Box::leak(Box::new(
        SubgraphClient::new(
            http_client,
            options
                .config
                .subgraphs
                .escrow
                .config
                .deployment_id
                .map(|deployment| {
                    DeploymentDetails::for_graph_node_url(
                        options.config.graph_node.status_url.clone(),
                        options.config.graph_node.query_url.clone(),
                        deployment,
                    )
                })
                .transpose()
                .expect("Failed to parse graph node query endpoint and escrow subgraph deployment"),
            DeploymentDetails::for_query_url_with_token(
                options.config.subgraphs.escrow.config.query_url.as_ref(),
                options
                    .config
                    .subgraphs
                    .escrow
                    .config
                    .query_auth_token
                    .clone(),
            )?,
        )
        .await,
    ));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        options.config.indexer.indexer_address,
        options.config.subgraphs.escrow.config.syncing_interval_secs,
        true, // Reject thawing signers eagerly
    )
    .await
    .expect("Error creating escrow_accounts channel");

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema
    // Note: Typically, you'd call `sqlx::migrate!();` here to sync the models
    // which defaults to files in  "./migrations" to sync the database;
    // however, this can cause conflicts with the migrations run by indexer
    // agent. Hence we leave syncing and migrating entirely to the agent and
    // assume the models are up to date in the service.
    let database = PgPoolOptions::new()
        .max_connections(DATABASE_MAX_CONNECTIONS)
        .acquire_timeout(DATABASE_TIMEOUT)
        .connect(
            options
                .config
                .database
                .clone()
                .get_formated_postgres_url()
                .as_ref(),
        )
        .await?;

    let domain_separator = tap_eip712_domain(
        options.config.blockchain.chain_id as u64,
        options.config.blockchain.receipts_verifier_address,
    );
    let indexer_context = IndexerTapContext::new(database.clone(), domain_separator.clone()).await;
    let timestamp_error_tolerance = options.config.tap.rav_request.timestamp_buffer_secs;

    let receipt_max_value = options.config.service.tap.max_receipt_value_grt.get_value();

    let checks = IndexerTapContext::get_checks(
        database,
        allocations.clone(),
        escrow_accounts.clone(),
        timestamp_error_tolerance,
        receipt_max_value,
    )
    .await;

    let tap_manager = Box::leak(Box::new(Manager::new(
        domain_separator.clone(),
        indexer_context,
        CheckList::new(checks),
    )));

    // Rate limits by allowing bursts of 10 requests and requiring 100ms of
    // time between consecutive requests after that, effectively rate
    // limiting to 10 req/s.
    let misc_rate_limiter = GovernorLayer {
        config: Arc::new(
            GovernorConfigBuilder::default()
                .per_millisecond(MISC_BURST_PER_MILLISECOND)
                .burst_size(MISC_BURST_SIZE)
                .finish()
                .expect("Failed to set up rate limiting"),
        ),
    };

    let operator_address = Json(
        serde_json::json!({ "publicKey": public_key(&options.config.indexer.operator_mnemonic)?}),
    );

    let mut misc_routes = Router::new()
        .route("/", get("Service is up and running"))
        .route("/version", get(Json(options.release)))
        .route("/info", get(operator_address))
        .layer(misc_rate_limiter.clone());

    // Rate limits by allowing bursts of 50 requests and requiring 20ms of
    // time between consecutive requests after that, effectively rate
    // limiting to 50 req/s.
    let static_subgraph_rate_limiter = GovernorLayer {
        config: Arc::new(
            GovernorConfigBuilder::default()
                .per_millisecond(STATIC_BURST_PER_MILLISECOND)
                .burst_size(STATIC_BURST_SIZE)
                .finish()
                .expect("Failed to set up rate limiting"),
        ),
    };

    // Check subgraph Health
    misc_routes = misc_routes
        .route(
            "/subgraph/health/:deployment_id",
            get(health).with_state(options.config.graph_node.clone()),
        )
        .layer(misc_rate_limiter);

    if options.config.service.serve_network_subgraph {
        if let Some(free_auth_token) = &options.config.service.serve_auth_token {
            info!("Serving network subgraph at /network");

            let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);

            misc_routes = misc_routes.route(
                "/network",
                post(static_subgraph_request_handler)
                    .route_layer(auth_layer)
                    .with_state(network_subgraph)
                    .route_layer(static_subgraph_rate_limiter.clone()),
            );
        } else {
            warn!("`serve_network_subgraph` is enabled but no `serve_auth_token` provided. Disabling it.");
        }
    }

    if options.config.service.serve_escrow_subgraph {
        if let Some(free_auth_token) = &options.config.service.serve_auth_token {
            info!("Serving escrow subgraph at /escrow");

            let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);

            misc_routes = misc_routes.route(
                "/escrow",
                post(static_subgraph_request_handler)
                    .route_layer(auth_layer)
                    .with_state(escrow_subgraph)
                    .route_layer(static_subgraph_rate_limiter),
            )
        } else {
            warn!("`serve_escrow_subgraph` is enabled but no `serve_auth_token` provided. Disabling it.");
        }
    }

    let mut request_handler_route = post(request_handler);

    // inject auth
    let failed_receipt_metric = Box::leak(Box::new(FAILED_RECEIPT.clone()));
    let tap_auth = auth::tap_receipt_authorize(tap_manager, failed_receipt_metric);

    if let Some(free_auth_token) = &options.config.service.serve_auth_token {
        let free_query = Bearer::new(free_auth_token);
        let result = free_query.or(tap_auth);
        let auth_layer = AsyncRequireAuthorizationLayer::new(result);
        request_handler_route = request_handler_route.layer(auth_layer);
    } else {
        let auth_layer = AsyncRequireAuthorizationLayer::new(tap_auth);
        request_handler_route = request_handler_route.layer(auth_layer);
    }

    let deployment_to_allocation = deployment_to_allocation(allocations);
    let allocation_state = AllocationState {
        deployment_to_allocation,
    };
    let sender_state = SenderState {
        escrow_accounts,
        domain_separator,
    };
    let attestation_state = AttestationState {
        attestation_signers,
    };

    let service_builder = ServiceBuilder::new()
        // inject deployment id
        .layer(from_fn(deployment_middleware))
        // inject receipt
        .layer(from_fn(receipt_middleware))
        // inject allocation id
        .layer(from_fn_with_state(allocation_state, allocation_middleware))
        // inject sender
        .layer(from_fn_with_state(sender_state, sender_middleware))
        // inject metrics labels
        .layer(from_fn(labels_middleware))
        // metrics for histogram and failure
        .layer(PrometheusMetricsMiddlewareLayer::new(
            HANDLER_HISTOGRAM.clone(),
            HANDLER_FAILURE.clone(),
        ))
        // tap context
        .layer(from_fn(context_middleware))
        // inject signer
        .layer(from_fn_with_state(attestation_state, signer_middleware))
        // create attestation
        .layer(from_fn(attestation_middleware));

    request_handler_route = request_handler_route.layer(service_builder);

    let data_routes = Router::new()
        .route(
            PathBuf::from(&options.config.service.url_prefix)
                .join(format!("{}/id/:id", options.url_namespace))
                .to_str()
                .expect("Failed to set up `/{url_namespace}/id/:id` route"),
            request_handler_route,
        )
        .with_state(options.subgraph_state.clone());

    let router = NormalizePath::trim_trailing_slash(
        misc_routes
            .merge(data_routes)
            .merge(options.extra_routes)
            .layer(
                CorsLayer::new()
                    .allow_origin(cors::Any)
                    .allow_headers(cors::Any)
                    .allow_methods([Method::OPTIONS, Method::POST, Method::GET]),
            )
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(|req: &Request<_>| {
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
            .with_state(options.subgraph_state),
    );

    serve_metrics(options.config.metrics.get_socket_addr());

    info!(
        address = %options.config.service.host_and_port,
        "Serving requests",
    );
    let listener = TcpListener::bind(&options.config.service.host_and_port)
        .await
        .expect("Failed to bind to indexer-service port");

    Ok(serve(
        listener,
        ServiceExt::<ExtractRequest>::into_make_service_with_connect_info::<SocketAddr>(router),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?)
}

fn serve_metrics(host_and_port: SocketAddr) {
    info!(address = %host_and_port, "Serving prometheus metrics");

    tokio::spawn(async move {
        let router = Router::new().route(
            "/metrics",
            get(|| async {
                let metric_families = prometheus::gather();
                let encoder = TextEncoder::new();

                match encoder.encode_to_string(&metric_families) {
                    Ok(s) => (StatusCode::OK, s),
                    Err(e) => {
                        error!("Error encoding metrics: {}", e);
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error encoding metrics: {}", e),
                        )
                    }
                }
            }),
        );

        serve(
            TcpListener::bind(host_and_port)
                .await
                .expect("Failed to bind to metrics port"),
            router.into_make_service(),
        )
        .await
        .expect("Failed to serve metrics")
    });
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
