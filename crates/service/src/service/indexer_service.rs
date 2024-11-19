// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::dyn_abi::Eip712Domain;
use anyhow;
use axum::extract::MatchedPath;
use axum::extract::Request as ExtractRequest;
use axum::http::{Method, Request};
use axum::{
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum::{serve, ServiceExt};
use build_info::BuildInfo;
use indexer_common::{
    attestation::AttestationSigner,
    client::{DeploymentDetails, SubgraphClient},
    escrow_accounts::{EscrowAccounts, EscrowAccountsError},
    monitors::{attestation_signers, dispute_manager, escrow_accounts, indexer_allocations},
    wallet::public_key,
};
use prometheus::TextEncoder;
use reqwest::StatusCode;
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap, error::Error, fmt::Debug, net::SocketAddr, path::PathBuf, sync::Arc,
    time::Duration,
};
use tap_core::{manager::Manager, receipt::checks::CheckList, tap_eip712_domain};
use thegraph_core::{Address, Attestation};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::watch::Receiver;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::validate_request::ValidateRequestHeaderLayer;
use tower_http::{cors, cors::CorsLayer, normalize_path::NormalizePath, trace::TraceLayer};
use tracing::warn;
use tracing::{error, info, info_span};

use crate::error::SubgraphServiceError;
use crate::routes::health;
use crate::routes::request_handler;
use crate::routes::static_subgraph_request_handler;
use crate::tap::IndexerTapContext;
use indexer_config::Config;

use super::SubgraphService;

pub trait IndexerServiceResponse {
    type Data: IntoResponse;
    type Error: Error;

    fn is_attestable(&self) -> bool;
    fn as_str(&self) -> Result<&str, Self::Error>;
    fn finalize(self, attestation: AttestationOutput) -> Self::Data;
}

pub enum AttestationOutput {
    Attestation(Option<Attestation>),
    Attestable,
}

#[derive(Debug, Error)]
pub enum IndexerServiceError {
    #[error("Issues with provided receipt: {0}")]
    ReceiptError(tap_core::Error),
    #[error("No attestation signer found for allocation `{0}`")]
    NoSignerForAllocation(Address),
    #[error("Invalid request body: {0}")]
    InvalidRequest(anyhow::Error),
    #[error("Error while processing the request: {0}")]
    ProcessingError(SubgraphServiceError),
    #[error("No valid receipt or free query auth token provided")]
    Unauthorized,
    #[error("Invalid free query auth token")]
    InvalidFreeQueryAuthToken,
    #[error("Failed to sign attestation")]
    FailedToSignAttestation,

    #[error("Could not decode signer: {0}")]
    CouldNotDecodeSigner(tap_core::Error),

    #[error("There was an error while accessing escrow account: {0}")]
    EscrowAccount(EscrowAccountsError),
}

impl IntoResponse for IndexerServiceError {
    fn into_response(self) -> Response {
        use IndexerServiceError::*;

        #[derive(Serialize)]
        struct ErrorResponse {
            message: String,
        }

        let status = match self {
            Unauthorized => StatusCode::UNAUTHORIZED,

            NoSignerForAllocation(_) | FailedToSignAttestation => StatusCode::INTERNAL_SERVER_ERROR,

            ReceiptError(_)
            | InvalidRequest(_)
            | InvalidFreeQueryAuthToken
            | CouldNotDecodeSigner(_)
            | EscrowAccount(_)
            | ProcessingError(_) => StatusCode::BAD_REQUEST,
        };
        tracing::error!(%self, "An IndexerServiceError occoured.");
        (
            status,
            Json(ErrorResponse {
                message: self.to_string(),
            }),
        )
            .into_response()
    }
}

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
    pub service_impl: SubgraphService,
    pub config: &'static Config,
    pub release: IndexerServiceRelease,
    pub url_namespace: &'static str,
    pub extra_routes: Router<Arc<IndexerServiceState>>,
}

pub struct IndexerServiceState {
    pub config: Config,
    pub attestation_signers: Receiver<HashMap<Address, AttestationSigner>>,
    pub tap_manager: Manager<IndexerTapContext>,
    pub service_impl: SubgraphService,

    // tap
    pub escrow_accounts: Receiver<EscrowAccounts>,
    pub domain_separator: Eip712Domain,
}

pub struct IndexerService {}

impl IndexerService {
    pub async fn run(options: IndexerServiceOptions) -> Result<(), anyhow::Error> {
        let http_client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(30))
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
        let dispute_manager = dispute_manager(network_subgraph, Duration::from_secs(3600))
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
                    .expect(
                        "Failed to parse graph node query endpoint and escrow subgraph deployment",
                    ),
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
            .max_connections(50)
            .acquire_timeout(Duration::from_secs(30))
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
        let indexer_context =
            IndexerTapContext::new(database.clone(), domain_separator.clone()).await;
        let timestamp_error_tolerance = options.config.tap.rav_request.timestamp_buffer_secs;

        let receipt_max_value = options.config.service.tap.max_receipt_value_grt.get_value();

        let checks = IndexerTapContext::get_checks(
            database,
            allocations,
            escrow_accounts.clone(),
            domain_separator.clone(),
            timestamp_error_tolerance,
            receipt_max_value,
        )
        .await;

        let tap_manager = Manager::new(
            domain_separator.clone(),
            indexer_context,
            CheckList::new(checks),
        );

        let state = Arc::new(IndexerServiceState {
            config: options.config.clone(),
            attestation_signers,
            tap_manager,
            service_impl: options.service_impl,
            escrow_accounts,
            domain_separator,
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
                    .per_millisecond(20)
                    .burst_size(50)
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

        misc_routes = misc_routes.with_state(state.clone());

        let data_routes = Router::new()
            .route(
                PathBuf::from(&options.config.service.url_prefix)
                    .join(format!("{}/id/:id", options.url_namespace))
                    .to_str()
                    .expect("Failed to set up `/{url_namespace}/id/:id` route"),
                post(request_handler),
            )
            .with_state(state.clone());

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
                .with_state(state),
        );

        Self::serve_metrics(options.config.metrics.get_socket_addr());

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
