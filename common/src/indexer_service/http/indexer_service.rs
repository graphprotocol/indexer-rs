use std::{
    collections::HashMap, fmt::Debug, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration,
};

use alloy_primitives::Address;
use alloy_sol_types::eip712_domain;
use anyhow;
use autometrics::prometheus_exporter;
use axum::{
    async_trait,
    body::Body,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router, Server,
};
use build_info::BuildInfo;
use eventuals::Eventual;
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::postgres::PgPoolOptions;
use thegraph::types::DeploymentId;
use thiserror::Error;
use tokio::signal;
use tracing::info;

use crate::{
    prelude::{
        attestation_signers, dispute_manager, escrow_accounts, indexer_allocations,
        AttestationSigner, DeploymentDetails, SubgraphClient,
    },
    tap_manager::TapManager,
};

use super::{request_handler::request_handler, IndexerServiceConfig};

pub trait IsAttestable {
    fn is_attestable(&self) -> bool;
}

#[async_trait]
pub trait IndexerServiceImpl {
    type Error: std::error::Error;
    type Request: DeserializeOwned + Send + Debug + Serialize;
    type Response: IntoResponse + Serialize + IsAttestable;
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
    #[error("No receipt or free query auth token provided")]
    Unauthorized,
    #[error("Invalid free query auth token: {0}")]
    InvalidFreeQueryAuthToken(String),
    #[error("Failed to sign attestation")]
    FailedToSignAttestation,
    #[error("Failed to provide attestation")]
    FailedToProvideAttestation,
    #[error("Failed to provide response")]
    FailedToProvideResponse,
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
            InvalidFreeQueryAuthToken(_) => StatusCode::BAD_REQUEST,
            ProcessingError(_) => StatusCode::BAD_REQUEST,
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
}

pub struct IndexerService {}

impl IndexerService {
    pub async fn run<I>(options: IndexerServiceOptions<I>) -> Result<(), anyhow::Error>
    where
        I: IndexerServiceImpl + Sync + Send + 'static,
    {
        let network_subgraph = Box::leak(Box::new(SubgraphClient::new(
            options
                .config
                .graph_node
                .as_ref()
                .zip(options.config.network_subgraph.deployment)
                .map(|(graph_node, deployment)| {
                    DeploymentDetails::for_graph_node(&graph_node.query_base_url, deployment)
                })
                .transpose()?,
            DeploymentDetails::for_query_url(&options.config.network_subgraph.query_url)?,
        )?));

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
            options.config.graph_network.id.into(),
            dispute_manager,
        );

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            options
                .config
                .graph_node
                .as_ref()
                .zip(options.config.escrow_subgraph.deployment)
                .map(|(graph_node, deployment)| {
                    DeploymentDetails::for_graph_node(&graph_node.query_base_url, deployment)
                })
                .transpose()?,
            DeploymentDetails::for_query_url(&options.config.escrow_subgraph.query_url)?,
        )?));

        let escrow_accounts = escrow_accounts(
            escrow_subgraph,
            options.config.indexer.indexer_address,
            Duration::from_secs(options.config.escrow_subgraph.syncing_interval),
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
            // TODO: arguments for eip712_domain should be a config
            eip712_domain! {
                name: "TapManager",
                version: "1",
                verifying_contract: options.config.indexer.indexer_address,
            },
        );

        let state = Arc::new(IndexerServiceState {
            config: options.config.clone(),
            attestation_signers,
            tap_manager,
            service_impl: Arc::new(options.service_impl),
        });

        let router = Router::new()
            .route("/", get("Service is up and running"))
            .route("/version", get(Json(options.release)))
            .route(
                PathBuf::from(options.config.server.url_prefix)
                    .join("manifests/:id")
                    .to_str()
                    .expect("Failed to set up `/manifest/:id` route"),
                post(request_handler::<I>),
            )
            .merge(options.extra_routes)
            .with_state(state);

        Self::serve_metrics(options.config.server.metrics_host_and_port);

        info!(
            address = %options.config.server.host_and_port,
            "Serving requests",
        );

        Ok(Server::bind(&options.config.server.host_and_port)
            .serve(router.into_make_service())
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
