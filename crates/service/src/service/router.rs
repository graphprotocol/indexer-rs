// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use async_graphql_axum::GraphQL;
use axum::{
    extract::MatchedPath,
    http::Request,
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post, post_service},
    Json, Router,
};
use governor::{clock::QuantaInstant, middleware::NoOpMiddleware};
use indexer_config::{
    BlockchainConfig, EscrowSubgraphConfig, GraphNodeConfig, IndexerConfig, NetworkSubgraphConfig,
    ServiceConfig, ServiceTapConfig,
};
use indexer_monitor::{
    attestation_signers, deployment_to_allocation, dispute_manager, escrow_accounts_v1,
    escrow_accounts_v2, indexer_allocations, AllocationWatcher, DisputeManagerWatcher,
    EscrowAccountsWatcher, SubgraphClient,
};
use reqwest::Method;
use tap_core::{manager::Manager, receipt::checks::CheckList};
use thegraph_core::alloy::sol_types::Eip712Domain;
use tower::ServiceBuilder;
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer,
};
use tower_http::{
    auth::AsyncRequireAuthorizationLayer,
    cors::{self, CorsLayer},
    trace::TraceLayer,
    validate_request::ValidateRequestHeaderLayer,
};

use super::{release::IndexerServiceRelease, GraphNodeState};
use crate::{
    metrics::{FAILED_RECEIPT, HANDLER_HISTOGRAM},
    middleware::{
        allocation_middleware, attestation_middleware,
        auth::{self, Bearer, OrExt},
        context_middleware, deployment_middleware, labels_middleware, receipt_middleware,
        sender_middleware, signer_middleware, AllocationState, AttestationState,
        PrometheusMetricsMiddlewareLayer, SenderState, TapContextState,
    },
    routes::{self, health, request_handler, static_subgraph_request_handler},
    tap::{IndexerTapContext, TapChecksConfig},
    wallet::public_key,
};

#[derive(bon::Builder)]
pub struct ServiceRouter {
    // database
    database: sqlx::PgPool,
    // tap domain
    domain_separator: Eip712Domain,
    // tap domain v2
    domain_separator_v2: Eip712Domain,

    // graphnode client
    http_client: reqwest::Client,
    // release info
    release: Option<IndexerServiceRelease>,

    // configuration
    graph_node: GraphNodeConfig,
    indexer: IndexerConfig,
    service: ServiceConfig,
    blockchain: BlockchainConfig,
    timestamp_buffer_secs: Duration,

    // either provide subgraph or watcher
    #[builder(with =
        |subgraph: &'static SubgraphClient,
        config: EscrowSubgraphConfig|
        (subgraph, config))]
    escrow_subgraph: Option<(&'static SubgraphClient, EscrowSubgraphConfig)>,
    // V1_LEGACY: Legacy escrow watcher (used in legacy/hybrid mode)
    escrow_accounts_v1: Option<EscrowAccountsWatcher>,

    escrow_accounts_v2: Option<EscrowAccountsWatcher>,

    // provide network subgraph or allocations + dispute manager
    #[builder(with = |subgraph: &'static SubgraphClient,
        config: NetworkSubgraphConfig|
        (subgraph, config))]
    network_subgraph: Option<(&'static SubgraphClient, NetworkSubgraphConfig)>,
    allocations: Option<AllocationWatcher>,
    dispute_manager: Option<DisputeManagerWatcher>,
}

const MISC_BURST_SIZE: u32 = 10;
/// Replenish interval in milliseconds. 100ms = 10 req/s after burst is exhausted.
const MISC_REPLENISH_INTERVAL_MS: u64 = 100;

const STATIC_BURST_SIZE: u32 = 50;
/// Replenish interval in milliseconds. 20ms = 50 req/s after burst is exhausted.
const STATIC_REPLENISH_INTERVAL_MS: u64 = 20;

const DISPUTE_MANAGER_INTERVAL: Duration = Duration::from_secs(3600);

const DEFAULT_ROUTE: &str = "/";

impl ServiceRouter {
    pub async fn create_router(self) -> anyhow::Result<Router> {
        let indexer_address = self.indexer.indexer_address;
        let operator_mnemonics = self.indexer.get_operator_mnemonics();
        tracing::info!(
            mnemonic_count = operator_mnemonics.len(),
            "Loaded operator mnemonics for attestation signing"
        );
        let ServiceConfig {
            serve_network_subgraph,
            serve_escrow_subgraph,
            serve_auth_token,
            url_prefix,
            tap: ServiceTapConfig {
                max_receipt_value_grt,
            },
            free_query_auth_token,
            max_cost_model_batch_size,
            max_request_body_size,
            ..
        } = self.service;

        // COST
        let cost_schema =
            routes::cost::build_schema(self.database.clone(), max_cost_model_batch_size).await;
        let post_cost = post_service(GraphQL::new(cost_schema));

        // STATUS
        let post_status = post(routes::status);

        // Monitor the indexer's own allocations
        // if not provided, create monitor from subgraph
        let allocations = match (self.allocations, self.network_subgraph.as_ref()) {
            (Some(allocations), _) => allocations,
            (_, Some((network_subgraph, network))) => indexer_allocations(
                network_subgraph,
                indexer_address,
                network.config.syncing_interval_secs,
                network.recently_closed_allocation_buffer_secs,
            )
            .await
            .expect("Failed to initialize indexer_allocations watcher"),
            (None, None) => panic!("No allocations or network subgraph was provided"),
        };

        // V1_LEGACY: Monitor escrow accounts v1 (legacy/hybrid)
        // if not provided, create monitor from subgraph
        let escrow_accounts_v1 = match (self.escrow_accounts_v1, self.escrow_subgraph.as_ref()) {
            (Some(escrow_account), _) => Some(escrow_account),
            (_, Some((escrow_subgraph, escrow))) => Some(
                escrow_accounts_v1(
                    escrow_subgraph,
                    indexer_address,
                    escrow.config.syncing_interval_secs,
                    true, // Reject thawing signers eagerly
                )
                .await
                .expect("Error creating escrow_accounts_v1 channel"),
            ),
            (None, None) => None,
        };

        // Monitor escrow accounts v2
        // if not provided, create monitor from subgraph
        let escrow_accounts_v2 = match (self.escrow_accounts_v2, self.escrow_subgraph.as_ref()) {
            (Some(escrow_account), _) => Some(escrow_account),
            (_, Some((escrow_subgraph, escrow))) => Some(
                escrow_accounts_v2(
                    escrow_subgraph,
                    indexer_address,
                    escrow.config.syncing_interval_secs,
                    true, // Reject thawing signers eagerly
                )
                .await
                .expect("Error creating escrow_accounts_v2 channel"),
            ),
            (None, None) => None,
        };

        // Ensure at least one escrow accounts watcher is available
        if escrow_accounts_v1.is_none() && escrow_accounts_v2.is_none() {
            panic!("At least one escrow accounts watcher (v1 or v2) must be provided");
        }

        // Monitor dispute manager address
        // if not provided, create monitor from subgraph
        let dispute_manager = match (self.dispute_manager, self.network_subgraph.as_ref()) {
            (Some(dispute_manager), _) => dispute_manager,
            (_, Some((network_subgraph, _))) => {
                dispute_manager(network_subgraph, DISPUTE_MANAGER_INTERVAL)
                    .await
                    .expect("Failed to initialize dispute manager")
            }
            (None, None) => panic!("No dispute allocations or network subgraph was provided"),
        };

        // Maintain an up-to-date set of attestation signers, one for each
        // allocation. Multiple mnemonics are tried to support allocations
        // created with different operator keys.
        let attestation_signers = attestation_signers(
            allocations.clone(),
            operator_mnemonics.clone(),
            self.blockchain.chain_id as u64,
            dispute_manager,
        );

        // Rate limits by allowing bursts of 10 requests and requiring 100ms of
        // time between consecutive requests after that, effectively rate
        // limiting to 10 req/s.
        let misc_rate_limiter = create_rate_limiter(MISC_REPLENISH_INTERVAL_MS, MISC_BURST_SIZE);

        // Rate limits by allowing bursts of 50 requests and requiring 20ms of
        // time between consecutive requests after that, effectively rate
        // limiting to 50 req/s.
        let static_subgraph_rate_limiter =
            create_rate_limiter(STATIC_REPLENISH_INTERVAL_MS, STATIC_BURST_SIZE);

        // load serve_network_subgraph route
        let serve_network_subgraph = match (
            serve_auth_token.as_ref(),
            serve_network_subgraph,
            self.network_subgraph.as_ref(),
        ) {
            (Some(free_auth_token), true, Some((network_subgraph, _))) => {
                tracing::info!("Serving network subgraph at /network");

                let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);

                Router::new().route(
                    DEFAULT_ROUTE,
                    post(static_subgraph_request_handler)
                        .route_layer(auth_layer)
                        .route_layer(static_subgraph_rate_limiter.clone())
                        .with_state(network_subgraph),
                )
            }
            (_, true, _) => {
                tracing::warn!("`serve_network_subgraph` is enabled but no `serve_auth_token` provided. Disabling it.");
                Router::new()
            }
            _ => Router::new(),
        };

        // load serve_escrow_subgraph route
        let serve_escrow_subgraph = match (
            serve_auth_token.as_ref(),
            serve_escrow_subgraph,
            self.escrow_subgraph,
        ) {
            (Some(free_auth_token), true, Some((escrow_subgraph, _))) => {
                tracing::info!("Serving escrow subgraph at /escrow");

                let auth_layer = ValidateRequestHeaderLayer::bearer(free_auth_token);

                Router::new().route(
                    DEFAULT_ROUTE,
                    post(static_subgraph_request_handler)
                        .route_layer(auth_layer)
                        .route_layer(static_subgraph_rate_limiter)
                        .with_state(escrow_subgraph),
                )
            }
            (_, true, _) => {
                tracing::warn!("`serve_escrow_subgraph` is enabled but no `serve_auth_token` provided. Disabling it.");
                Router::new()
            }
            _ => Router::new(),
        };

        let post_request_handler = {
            // Create tap manager to validate receipts
            let (tap_manager_v1, tap_manager_v2) = {
                // Create context
                let indexer_context = IndexerTapContext::new(
                    self.database.clone(),
                    self.domain_separator.clone(),
                    self.domain_separator_v2.clone(),
                )
                .await;

                let timestamp_error_tolerance = self.timestamp_buffer_secs;
                let receipt_max_value = max_receipt_value_grt.get_value();

                // Create checks
                let allowed_data_services = self
                    .blockchain
                    .subgraph_service_address
                    .map(|addr| vec![addr]);

                let checks = IndexerTapContext::get_checks(TapChecksConfig {
                    pgpool: self.database,
                    indexer_allocations: allocations.clone(),
                    escrow_accounts_v1: escrow_accounts_v1.clone(),
                    escrow_accounts_v2: escrow_accounts_v2.clone(),
                    timestamp_error_tolerance,
                    receipt_max_value,
                    allowed_data_services,
                    service_provider: self.indexer.indexer_address,
                })
                .await;

                // Returned static Manager
                let m1 = Arc::new(Manager::new(
                    self.domain_separator.clone(),
                    indexer_context.clone(),
                    CheckList::new(checks.clone()),
                ));
                let m2 = Arc::new(Manager::new(
                    self.domain_separator_v2.clone(),
                    indexer_context,
                    CheckList::new(checks),
                ));

                (m1, m2)
            };

            let attestation_state = AttestationState {
                attestation_signers,
            };

            let mut handler = post(request_handler);

            handler = handler
                // create attestation
                .route_layer(from_fn(attestation_middleware))
                // inject signer
                .route_layer(from_fn_with_state(attestation_state, signer_middleware));

            // inject auth
            let failed_receipt_metric = Box::leak(Box::new(FAILED_RECEIPT.clone()));
            // let tap_auth = auth::tap_receipt_authorize(tap_manager, failed_receipt_metric);
            let tap_auth = auth::dual_tap_receipt_authorize(
                tap_manager_v1,
                tap_manager_v2,
                failed_receipt_metric,
            );

            if let Some(free_auth_token) = &free_query_auth_token {
                let free_query = Bearer::new(free_auth_token);
                let result = free_query.or(tap_auth);
                let auth_layer = AsyncRequireAuthorizationLayer::new(result);
                handler = handler.route_layer(auth_layer);
            } else {
                let auth_layer = AsyncRequireAuthorizationLayer::new(tap_auth);
                handler = handler.route_layer(auth_layer);
            }

            let deployment_to_allocation = deployment_to_allocation(allocations);
            let allocation_state = AllocationState {
                deployment_to_allocation,
            };
            let sender_state = SenderState {
                escrow_accounts_v1,
                escrow_accounts_v2,
                domain_separator: self.domain_separator,
                domain_separator_v2: self.domain_separator_v2,
            };

            let tap_context_state = TapContextState {
                max_request_body_size,
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
                ))
                // tap context (with body size limit for DoS protection)
                .layer(from_fn_with_state(tap_context_state, context_middleware));

            handler.route_layer(service_builder)
        };

        // setup cors
        let cors_layer = CorsLayer::new()
            .allow_origin(cors::Any)
            .allow_headers(cors::Any)
            .allow_methods([Method::OPTIONS, Method::POST, Method::GET]);

        // add tracing to all routes
        let tracing_layer = TraceLayer::new_for_http()
            .make_span_with(|req: &Request<_>| {
                let method = req.method();
                let uri = req.uri();
                let matched_path = req
                    .extensions()
                    .get::<MatchedPath>()
                    .map(MatchedPath::as_str);

                tracing::info_span!(
                    "http_request",
                    %method,
                    %uri,
                    matched_path,
                )
            })
            // we disable failures here because we are doing our own error logging
            .on_failure(
                |_error: tower_http::classify::ServerErrorsFailureClass,
                 _latency: Duration,
                 _span: &tracing::Span| {},
            );

        let version = match self.release {
            Some(release) => Router::new().route(DEFAULT_ROUTE, get(Json(release))),
            None => Router::new(),
        };

        // The /info endpoint displays the operator's public key. When multiple operator
        // mnemonics are configured (for supporting allocations created with different keys),
        // only the first mnemonic's public key is displayed. This represents the "primary"
        // operator identity. All mnemonics are still used internally for attestation signing.
        let primary_mnemonic = operator_mnemonics.first().ok_or_else(|| {
            anyhow::anyhow!(
                "No operator mnemonic configured. This should have been caught during config validation."
            )
        })?;
        let operator_address =
            Json(serde_json::json!({ "publicKey": public_key(primary_mnemonic)?}));

        // Graph node state
        let graphnode_state = GraphNodeState {
            graph_node_client: self.http_client,
            graph_node_status_url: self.graph_node.status_url,
            graph_node_query_base_url: self.graph_node.query_url,
        };

        // data layer
        let data_routes = Router::new()
            .route("/subgraphs/id/{id}", post_request_handler)
            .with_state(graphnode_state.clone());

        // If url_prefix == "/", use merge; otherwise, it's safe to nest
        let subgraphs_route = if url_prefix == "/" {
            data_routes
        } else {
            Router::new().nest(&url_prefix, data_routes)
        };

        let misc_routes = Router::new()
            .route("/", get("Service is up and running"))
            .route("/info", get(operator_address))
            .nest("/version", version)
            .nest("/escrow", serve_escrow_subgraph)
            .nest("/network", serve_network_subgraph)
            .route(
                "/subgraph/health/{deployment_id}",
                get(health).with_state(graphnode_state.clone()),
            )
            .layer(misc_rate_limiter);

        let extra_routes = Router::new()
            .route("/cost", post_cost)
            .route("/status", post_status.with_state(graphnode_state));

        let router = Router::new()
            .merge(misc_routes)
            .merge(subgraphs_route)
            .merge(extra_routes)
            .layer(cors_layer)
            .layer(tracing_layer);

        Ok(router)
    }
}

fn create_rate_limiter(
    replenish_interval_ms: u64,
    burst_size: u32,
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware<QuantaInstant>> {
    GovernorLayer {
        config: Arc::new(
            GovernorConfigBuilder::default()
                .per_millisecond(replenish_interval_ms)
                .burst_size(burst_size)
                .key_extractor(SmartIpKeyExtractor)
                .finish()
                .expect("Failed to set up rate limiting"),
        ),
    }
}
