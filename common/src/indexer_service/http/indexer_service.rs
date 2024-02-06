// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap, error::Error, fmt::Debug, net::SocketAddr, path::PathBuf, sync::Arc,
    time::Duration,
};

use alloy_sol_types::eip712_domain;
use anyhow;
use autometrics::prometheus_exporter;
use axum::{
    async_trait,
    body::Body,
    error_handling::HandleErrorLayer,
    response::{IntoResponse, Response},
    routing::{get, post},
    BoxError, Extension, Json, Router, Server,
};
use build_info::BuildInfo;
use eventuals::Eventual;
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::postgres::PgPoolOptions;
use thegraph::types::Address;
use thegraph::types::{Attestation, DeploymentId};
use thiserror::Error;
use tokio::signal;
use tower::ServiceBuilder;
use tower_governor::{errors::display_error, governor::GovernorConfigBuilder, GovernorLayer};
use tracing::info;

use crate::{
    indexer_service::http::{
        metrics::IndexerServiceMetrics, static_subgraph::static_subgraph_request_handler,
    },
    prelude::{
        attestation_signers, dispute_manager, escrow_accounts, indexer_allocations,
        AttestationSigner, DeploymentDetails, SubgraphClient,
    },
    tap_manager::TapManager,
};

use super::{request_handler::request_handler, IndexerServiceConfig};

pub trait IndexerServiceResponse {
    type Data: IntoResponse;
    type Error: Error;

    fn is_attestable(&self) -> bool;
    fn as_str(&self) -> Result<&str, Self::Error>;
    fn finalize(self, attestation: Option<Attestation>) -> Self::Data;
}

#[async_trait]
pub trait IndexerServiceImpl {
    type Error: std::error::Error;
    type Request: DeserializeOwned + Send + Debug + Serialize;
    type Response: IndexerServiceResponse + Sized;
    type State: Send + Sync;

    async fn process_request(
        &self,
        manifest_id: DeploymentId,
        request: Self::Request,
    ) -> Result<(Self::Request, Self::Response), Self::Error>;
}

#[derive(Debug, Error)]
pub enum IndexerServiceError<E>
where
    E: std::error::Error,
{
    #[error("No receipt provided with the request")]
    NoReceipt,
    #[error("Issues with provided receipt: {0}")]
    ReceiptError(anyhow::Error),
    #[error("Service is not ready yet, try again in a moment")]
    ServiceNotReady,
    #[error("No attestation signer found for allocation `{0}`")]
    NoSignerForAllocation(Address),
    #[error("No attestation signer found for manifest `{0}`")]
    NoSignerForManifest(DeploymentId),
    #[error("Invalid request body: {0}")]
    InvalidRequest(anyhow::Error),
    #[error("Error while processing the request: {0}")]
    ProcessingError(E),
    #[error("No valid receipt or free query auth token provided")]
    Unauthorized,
    #[error("Invalid free query auth token")]
    InvalidFreeQueryAuthToken,
    #[error("Failed to sign attestation")]
    FailedToSignAttestation,
    #[error("Failed to provide attestation")]
    FailedToProvideAttestation,
    #[error("Failed to provide response")]
    FailedToProvideResponse,
    #[error("Failed to query subgraph: {0}")]
    FailedToQueryStaticSubgraph(anyhow::Error),
}

impl<E> From<&IndexerServiceError<E>> for StatusCode
where
    E: std::error::Error,
{
    fn from(err: &IndexerServiceError<E>) -> Self {
        use IndexerServiceError::*;

        match err {
            ServiceNotReady => StatusCode::SERVICE_UNAVAILABLE,

            NoReceipt => StatusCode::PAYMENT_REQUIRED,

            Unauthorized => StatusCode::UNAUTHORIZED,

            NoSignerForAllocation(_) => StatusCode::INTERNAL_SERVER_ERROR,
            NoSignerForManifest(_) => StatusCode::INTERNAL_SERVER_ERROR,
            FailedToSignAttestation => StatusCode::INTERNAL_SERVER_ERROR,
            FailedToProvideAttestation => StatusCode::INTERNAL_SERVER_ERROR,
            FailedToProvideResponse => StatusCode::INTERNAL_SERVER_ERROR,

            ReceiptError(_) => StatusCode::BAD_REQUEST,
            InvalidRequest(_) => StatusCode::BAD_REQUEST,
            InvalidFreeQueryAuthToken => StatusCode::BAD_REQUEST,
            ProcessingError(_) => StatusCode::BAD_REQUEST,

            FailedToQueryStaticSubgraph(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// Tell axum how to convert `RpcError` into a response.
impl<E> IntoResponse for IndexerServiceError<E>
where
    E: std::error::Error,
{
    fn into_response(self) -> Response {
        (StatusCode::from(&self), self.to_string()).into_response()
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

pub struct IndexerServiceOptions<I>
where
    I: IndexerServiceImpl + Sync + Send + 'static,
{
    pub service_impl: I,
    pub config: IndexerServiceConfig,
    pub release: IndexerServiceRelease,
    pub url_namespace: &'static str,
    pub metrics_prefix: &'static str,
    pub extra_routes: Router<Arc<IndexerServiceState<I>>, Body>,
}

pub struct IndexerServiceState<I>
where
    I: IndexerServiceImpl + Sync + Send + 'static,
{
    pub config: IndexerServiceConfig,
    pub attestation_signers: Eventual<HashMap<Address, AttestationSigner>>,
    pub tap_manager: TapManager,
    pub service_impl: Arc<I>,
    pub metrics: IndexerServiceMetrics,
}

pub struct IndexerService {}

impl IndexerService {
    pub async fn run<I>(options: IndexerServiceOptions<I>) -> Result<(), anyhow::Error>
    where
        I: IndexerServiceImpl + Sync + Send + 'static,
    {
        let metrics = IndexerServiceMetrics::new(options.metrics_prefix);

        let http_client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to init HTTP client");

        let network_subgraph: &'static SubgraphClient = Box::leak(Box::new(SubgraphClient::new(
            http_client.clone(),
            options
                .config
                .graph_node
                .as_ref()
                .zip(options.config.network_subgraph.deployment)
                .map(|(graph_node, deployment)| {
                    DeploymentDetails::for_graph_node(
                        &graph_node.status_url,
                        &graph_node.query_base_url,
                        deployment,
                    )
                })
                .transpose()?,
            DeploymentDetails::for_query_url(&options.config.network_subgraph.query_url)?,
        )));

        // Identify the dispute manager for the configured network
        let dispute_manager = dispute_manager(
            network_subgraph,
            options.config.graph_network.id,
            Duration::from_secs(3600),
        );

        // Monitor the indexer's own allocations
        let allocations = indexer_allocations(
            network_subgraph,
            options.config.indexer.indexer_address,
            options.config.graph_network.id,
            Duration::from_secs(options.config.network_subgraph.syncing_interval),
        );

        // Maintain an up-to-date set of attestation signers, one for each
        // allocation
        let attestation_signers = attestation_signers(
            allocations.clone(),
            options.config.indexer.operator_mnemonic.clone(),
            options.config.graph_network.chain_id.into(),
            dispute_manager,
        );

        let escrow_subgraph: &'static SubgraphClient = Box::leak(Box::new(SubgraphClient::new(
            http_client,
            options
                .config
                .graph_node
                .as_ref()
                .zip(options.config.escrow_subgraph.deployment)
                .map(|(graph_node, deployment)| {
                    DeploymentDetails::for_graph_node(
                        &graph_node.status_url,
                        &graph_node.query_base_url,
                        deployment,
                    )
                })
                .transpose()?,
            DeploymentDetails::for_query_url(&options.config.escrow_subgraph.query_url)?,
        )));

        let escrow_accounts = escrow_accounts(
            escrow_subgraph,
            options.config.indexer.indexer_address,
            Duration::from_secs(options.config.escrow_subgraph.syncing_interval),
            true, // Reject thawing signers eagerly
        );

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
            .connect(&options.config.database.postgres_url)
            .await?;

        let tap_manager = TapManager::new(
            database,
            allocations,
            escrow_accounts,
            eip712_domain! {
                name: "TAP",
                version: "1",
                chain_id: options.config.scalar.chain_id,
                verifying_contract: options.config.scalar.receipts_verifier_address,
            },
        )
        .await;

        let state = Arc::new(IndexerServiceState {
            config: options.config.clone(),
            attestation_signers,
            tap_manager,
            service_impl: Arc::new(options.service_impl),
            metrics,
        });

        // Rate limits by allowing bursts of 10 requests and requiring 100ms of
        // time between consecutive requests after that, effectively rate
        // limiting to 10 req/s.
        let misc_rate_limiter = GovernorLayer {
            config: Box::leak(Box::new(
                GovernorConfigBuilder::default()
                    .per_millisecond(100)
                    .burst_size(10)
                    .finish()
                    .expect("Failed to set up rate limiting"),
            )),
        };

        let mut misc_routes = Router::new()
            .route("/", get("Service is up and running"))
            .route("/version", get(Json(options.release)))
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(|e: BoxError| async move {
                        display_error(e)
                    }))
                    .layer(misc_rate_limiter),
            );

        // Rate limits by allowing bursts of 50 requests and requiring 20ms of
        // time between consecutive requests after that, effectively rate
        // limiting to 50 req/s.
        let static_subgraph_rate_limiter = GovernorLayer {
            config: Box::leak(Box::new(
                GovernorConfigBuilder::default()
                    .per_millisecond(20)
                    .burst_size(50)
                    .finish()
                    .expect("Failed to set up rate limiting"),
            )),
        };

        if options.config.network_subgraph.serve_subgraph {
            info!("Serving network subgraph at /network");

            misc_routes = misc_routes.route(
                "/network",
                post(static_subgraph_request_handler::<I>)
                    .route_layer(Extension(network_subgraph))
                    .route_layer(Extension(
                        options.config.network_subgraph.serve_auth_token.clone(),
                    ))
                    .route_layer(
                        ServiceBuilder::new()
                            .layer(HandleErrorLayer::new(|e: BoxError| async move {
                                display_error(e)
                            }))
                            .layer(static_subgraph_rate_limiter.clone()),
                    ),
            );
        }

        if options.config.escrow_subgraph.serve_subgraph {
            info!("Serving escrow subgraph at /escrow");

            misc_routes = misc_routes
                .route("/escrow", post(static_subgraph_request_handler::<I>))
                .route_layer(Extension(escrow_subgraph))
                .route_layer(Extension(
                    options.config.escrow_subgraph.serve_auth_token.clone(),
                ))
                .route_layer(
                    ServiceBuilder::new()
                        .layer(HandleErrorLayer::new(|e: BoxError| async move {
                            display_error(e)
                        }))
                        .layer(static_subgraph_rate_limiter),
                );
        }

        misc_routes = misc_routes.with_state(state.clone());

        let data_routes = Router::new()
            .route(
                PathBuf::from(options.config.server.url_prefix)
                    .join(format!("{}/id/:id", options.url_namespace))
                    .to_str()
                    .expect("Failed to set up `/{url_namespace}/id/:id` route"),
                post(request_handler::<I>),
            )
            .with_state(state.clone());

        let router = misc_routes
            .merge(data_routes)
            .merge(options.extra_routes)
            .with_state(state);

        Self::serve_metrics(options.config.server.metrics_host_and_port);

        info!(
            address = %options.config.server.host_and_port,
            "Serving requests",
        );

        Ok(Server::bind(&options.config.server.host_and_port)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown_signal())
            .await?)
    }

    fn serve_metrics(host_and_port: SocketAddr) {
        info!(address = %host_and_port, "Serving prometheus metrics");

        tokio::spawn(async move {
            let router = Router::new().route(
                "/metrics",
                get(|| async { prometheus_exporter::encode_http_response() }),
            );

            Server::bind(&host_and_port)
                .serve(router.into_make_service())
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
