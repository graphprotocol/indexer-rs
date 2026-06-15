// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context};
use axum::{extract::Request, serve, ServiceExt};
use clap::Parser;
use graph_networks_registry::NetworksRegistry;
use indexer_config::{Config, DipsConfig, GraphNodeConfig, SubgraphConfig};
use indexer_dips::{
    database::PsqlRcaStore,
    ipfs::IpfsClient,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::indexer_dips_service_server::{
        IndexerDipsService, IndexerDipsServiceServer,
    },
    server::{DipsServer, DipsServerContext},
    trusted_signers::{SubgraphTrustedSigners, TrustedSignerSource},
};
use indexer_monitor::{DeploymentDetails, SubgraphClient};
use release::IndexerServiceRelease;
use reqwest::Url;
use tap_core::tap_eip712_domain;
use thegraph_core::alloy::primitives::{Address, U256};
use tokio::{net::TcpListener, signal};
use tokio_util::sync::CancellationToken;
use tonic::transport::server::TcpConnectInfo;
use tower::ServiceBuilder;
use tower_governor::{
    errors::GovernorError, governor::GovernorConfigBuilder, key_extractor::KeyExtractor,
    GovernorLayer,
};
use tower_http::normalize_path::NormalizePath;
use tracing::info;

use crate::{
    cli::Cli,
    constants::HTTP_CLIENT_TIMEOUT,
    database,
    metrics::{serve_metrics, DIPS_ENABLED},
    routes::DipsInfoState,
};

mod grpc_error_to_response;
mod release;
mod router;
mod tap_receipt_header;

use grpc_error_to_response::GrpcErrorToResponseLayer;

pub use router::ServiceRouter;
pub use tap_receipt_header::TapHeader;

/// Format a wei value as a human-readable GRT string.
///
/// Converts wei (10^-18 GRT) to GRT with up to 18 decimal places,
/// trimming trailing zeros. For example:
/// - 1_000_000_000_000_000_000 wei -> "1"
/// - 1_500_000_000_000_000_000 wei -> "1.5"
/// - 500_000_000_000_000_000 wei -> "0.5"
fn format_grt(wei: u128) -> String {
    let whole = wei / 10u128.pow(18);
    let frac = wei % 10u128.pow(18);
    if frac == 0 {
        whole.to_string()
    } else {
        // Format with up to 18 decimal places, trimming trailing zeros
        let frac_str = format!("{:018}", frac);
        let trimmed = frac_str.trim_end_matches('0');
        format!("{}.{}", whole, trimmed)
    }
}

#[derive(Clone)]
pub struct GraphNodeState {
    pub graph_node_client: reqwest::Client,
    pub graph_node_status_url: Url,
    pub graph_node_query_base_url: Url,
}

/// Run the subgraph indexer service
pub async fn run() -> anyhow::Result<()> {
    // Parse command line and environment arguments
    let cli = Cli::parse();

    // Load the service configuration
    let config = Config::parse(indexer_config::ConfigPrefix::Service, cli.config.as_ref())
        .map_err(|e| {
            tracing::error!(
                config_path = %cli.config.unwrap_or_default().display(),
                error = %e,
                "Invalid configuration file; you can use --config to fill missing values",
            );
            anyhow!(e)
        })?;

    // Parse basic configurations
    build_info::build_info!(fn build_info);
    let release = IndexerServiceRelease::from(build_info());

    let http_client =
        create_http_client(HTTP_CLIENT_TIMEOUT, true).context("Failed to create HTTP client")?;

    let network_subgraph = create_subgraph_client(
        http_client.clone(),
        &config.graph_node,
        &config.subgraphs.network.config,
    )
    .await;

    // DIPs reads the agreement-manager role set from the indexing-payments
    // subgraph; build its client before the router consumes graph_node.
    let indexing_payments_subgraph = if config.dips.is_some() {
        let subgraph_config = config.subgraphs.indexing_payments.as_ref().ok_or_else(|| {
            anyhow!("subgraphs.indexing_payments must be set when DIPs is enabled")
        })?;
        Some(create_subgraph_client(http_client.clone(), &config.graph_node, subgraph_config).await)
    } else {
        None
    };

    // V2 escrow accounts are in the network subgraph, not a separate escrow_v2 subgraph

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema.
    //
    // This binary does not run migrations. By convention, the indexer-agent
    // (graphprotocol/indexer, TypeScript) owns schema migrations to avoid
    // conflicting DDL from two processes sharing one database. The SQL files
    // in indexer-rs/migrations/ exist for local development (`sqlx migrate
    // run`) and tests only -- they are not executed by any production binary.
    //
    // For new tables (e.g. pending_rca_proposals), a corresponding migration
    // must be added to the agent before the feature ships to production.
    let database =
        database::connect(config.database.clone().get_formated_postgres_url().as_ref()).await;

    let domain_separator_v2 = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        config.blockchain.horizon_receipts_verifier_address(),
        tap_core::TapVersion::V2,
    );
    let host_and_port = config.service.host_and_port;
    let indexer_address = config.indexer.indexer_address;
    // Captured before `config.blockchain` is moved into the router state below;
    // used to build the RCA EIP-712 domain for DIPs signer recovery.
    let blockchain_chain_id = config.blockchain.chain_id as u64;
    let ipfs_url = config.service.ipfs_url.clone();

    // V2 escrow accounts (used by DIPs) live in the network subgraph; no
    // separate escrow subgraph is queried.
    let collector_address = config.blockchain.receipts_verifier_address_v2;
    let escrow_min_balance_grt_wei = config.subgraphs.network.escrow_min_balance_grt_wei.clone();
    let max_signers_per_payer = config.subgraphs.network.max_signers_per_payer;

    let v2_watcher = match indexer_monitor::escrow_accounts_v2(
        network_subgraph,
        indexer_address,
        config.subgraphs.network.config.syncing_interval_secs,
        true, // Reject thawing signers eagerly
        collector_address,
        escrow_min_balance_grt_wei.clone(),
        max_signers_per_payer,
    )
    .await
    {
        Ok(watcher) => {
            tracing::info!("V2 escrow accounts successfully initialized from network subgraph");
            watcher
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to initialize V2 escrow accounts; service cannot continue",
            );
            std::process::exit(1);
        }
    };

    // Create a cancellation token for coordinated graceful shutdown
    let shutdown_token = CancellationToken::new();

    // DIPS: RecurringCollectionAgreement validation and storage. An init
    // failure here must not take down query serving: log it loudly and run
    // without DIPs instead. /dips/info and the metric reflect the outcome.
    let dips_info_state = match config.dips.as_ref() {
        Some(dips) => match start_dips(
            dips,
            blockchain_chain_id,
            indexing_payments_subgraph
                .expect("indexing_payments client is built whenever DIPs is enabled"),
            &ipfs_url,
            database.clone(),
            indexer_address,
            &shutdown_token,
        )
        .await
        {
            Ok(()) => {
                DIPS_ENABLED.set(1);
                Some(DipsInfoState::Available {
                    min_grt_per_30_days: dips
                        .min_grt_per_30_days
                        .iter()
                        .map(|(network, grt)| (network.clone(), format_grt(grt.wei())))
                        .collect(),
                    min_grt_per_billion_entities_per_30_days: format_grt(
                        dips.min_grt_per_billion_entities_per_30_days.wei(),
                    ),
                })
            }
            Err(err) => {
                DIPS_ENABLED.set(0);
                tracing::error!(
                    error = format!("{err:#}"),
                    "DIPs initialization failed; the service is running WITHOUT DIPs \
                     and will not receive indexing agreement proposals until restarted"
                );
                Some(DipsInfoState::Failed)
            }
        },
        None => None,
    };

    let router = ServiceRouter::builder()
        .database(database.clone())
        .domain_separator_v2(domain_separator_v2.clone())
        .graph_node(config.graph_node)
        .http_client(http_client)
        .release(release)
        .indexer(config.indexer)
        .service(config.service)
        .blockchain(config.blockchain)
        .timestamp_buffer_secs(config.tap.rav_request.timestamp_buffer_secs)
        .network_subgraph(network_subgraph, config.subgraphs.network)
        .escrow_accounts_v2(v2_watcher.clone())
        .maybe_dips_info(dips_info_state)
        .build();

    serve_metrics(config.metrics.get_socket_addr());

    tracing::info!(
        address = %host_and_port,
        "Serving requests",
    );

    let listener = TcpListener::bind(&host_and_port)
        .await
        .with_context(|| format!("Failed to bind to indexer-service port '{host_and_port}'"))?;

    let app = router.create_router().await?;
    let router = NormalizePath::trim_trailing_slash(app);
    //
    let service = ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(router);
    Ok(serve(listener, service)
        .with_graceful_shutdown(shutdown_handler(shutdown_token))
        .await?)
}

/// Initialize and start the DIPS gRPC server. Errors return to the caller,
/// which runs the service without DIPs rather than letting an optional
/// subsystem take down query serving.
async fn start_dips(
    dips: &DipsConfig,
    blockchain_chain_id: u64,
    indexing_payments_subgraph: &'static SubgraphClient,
    ipfs_url: &Url,
    database: sqlx::PgPool,
    indexer_address: Address,
    shutdown_token: &CancellationToken,
) -> anyhow::Result<()> {
    let DipsConfig {
        host,
        port,
        supported_networks,
        min_grt_per_30_days,
        min_grt_per_billion_entities_per_30_days,
        additional_networks,
        recurring_collector,
        rpc_url,
        role_refresh_interval_secs,
        role_failopen_grace_secs,
        role_subgraph_max_lag_secs,
        max_new_agreements_per_24h,
    } = dips;

    // The RecurringCollector address is the EIP-712 verifying contract used to
    // recover RCA signers; without it DIPs cannot authenticate proposals.
    let recurring_collector = (*recurring_collector).ok_or_else(|| {
        anyhow::anyhow!("dips.recurring_collector must be set when DIPs is enabled")
    })?;

    // Prefer the domain the deployed contract reports (EIP-5267) so it tracks
    // upgrades; without an RPC endpoint, fall back to built-in constants.
    let rca_domain = match rpc_url {
        Some(url) => {
            indexer_dips::fetch_rca_eip712_domain(
                url.as_str(),
                recurring_collector,
                blockchain_chain_id,
            )
            .await?
        }
        None => {
            tracing::info!("dips.rpc_url not set; using built-in RCA EIP-712 domain constants");
            indexer_dips::rca_eip712_domain(blockchain_chain_id, recurring_collector)
        }
    };

    if supported_networks.is_empty() {
        tracing::warn!(
            "DIPS enabled but no networks in dips.supported_networks. \
             All proposals will be rejected."
        );
    }

    tracing::info!(
        supported_networks = ?supported_networks,
        ipfs_url = %ipfs_url,
        max_new_agreements_per_24h = ?max_new_agreements_per_24h,
        "DIPs configuration loaded"
    );
    for (network, grt) in min_grt_per_30_days.iter() {
        tracing::info!(
            network = %network,
            min_grt_per_30_days_wei = %grt.wei(),
            "DIPs network pricing"
        );
    }
    tracing::info!(
        min_grt_per_billion_entities_per_30_days_wei = %min_grt_per_billion_entities_per_30_days.wei(),
        "DIPs entity pricing"
    );

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("Invalid DIPS host:port '{host}:{port}'"))?;

    // Shared counter of in-flight gRPC requests. The IPFS client reads
    // it to decide whether to use the full retry budget or fall back to
    // a single attempt when the service is under load.
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Initialize validation dependencies
    let ipfs_fetcher = Arc::new(IpfsClient::new(ipfs_url.as_str(), inflight.clone())?);
    let registry = Arc::new(
        NetworksRegistry::from_latest_version()
            .await
            .context("Failed to fetch NetworksRegistry for DIPS")?,
    );

    // Convert GRT/30days to wei/second for protocol compatibility.
    // Use ceiling division to protect indexers: configured minimums round UP,
    // ensuring indexers never accept less than their stated minimum.
    const SECONDS_PER_30_DAYS: u128 = 30 * 24 * 60 * 60;
    let tokens_per_second = min_grt_per_30_days
        .iter()
        .map(|(network, grt)| {
            let wei_per_second = grt.wei().div_ceil(SECONDS_PER_30_DAYS);
            (network.clone(), U256::from(wei_per_second))
        })
        .collect();

    // Entity pricing: config is per-billion-entities, convert to per-entity.
    // Ceiling division protects indexer from precision loss.
    let entity_divisor = SECONDS_PER_30_DAYS * 1_000_000_000;
    let tokens_per_entity_per_second = U256::from(
        min_grt_per_billion_entities_per_30_days
            .wei()
            .div_ceil(entity_divisor),
    );

    // Authorise proposal senders against the on-chain agreement-manager role set,
    // read from the indexing-payments subgraph.
    let trusted_signers: Arc<dyn TrustedSignerSource> = SubgraphTrustedSigners::spawn(
        indexing_payments_subgraph,
        Duration::from_secs(*role_refresh_interval_secs),
        Duration::from_secs(*role_failopen_grace_secs),
        Duration::from_secs(*role_subgraph_max_lag_secs),
    )
    .await;

    // Build server context
    let ctx = Arc::new(DipsServerContext {
        rca_store: Arc::new(PsqlRcaStore { pool: database }),
        ipfs_fetcher,
        price_calculator: Arc::new(PriceCalculator::new(
            supported_networks.clone(),
            tokens_per_second,
            tokens_per_entity_per_second,
        )),
        registry,
        additional_networks: Arc::new(additional_networks.clone()),
        rca_domain,
        trusted_signers,
        max_new_agreements_per_24h: *max_new_agreements_per_24h,
    });

    // Create DIPS server
    let server = DipsServer {
        ctx,
        expected_payee: indexer_address,
        inflight,
    };

    info!(
        address = %addr,
        "Starting DIPS gRPC server (RecurringCollectionAgreement validation)"
    );

    let dips_shutdown_token = shutdown_token.clone();
    tokio::spawn(async move {
        start_dips_server(addr, server, dips_shutdown_token.cancelled()).await;
    });
    Ok(())
}

/// Per-request timeout across the whole gRPC handler. Long enough to cover
/// the worst-case IPFS retry budget (190s) with headroom; short enough that
/// a hung handler doesn't pin a worker indefinitely.
const DIPS_REQUEST_TIMEOUT: Duration = Duration::from_secs(220);

/// Global token-bucket rate limit shared across all callers. Bounds the
/// total proposal throughput regardless of per-IP behaviour. Sized to
/// accommodate burst traffic from a single trusted dipper.
const DIPS_RATE_LIMIT_PER_SEC: u64 = 50;

/// Per-IP rate limit replenishment interval. 200ms per token gives a
/// sustained 5 requests per second per source IP, which is comfortable
/// headroom for a real dipper but quickly cuts off any single misbehaving
/// caller.
const DIPS_PER_IP_REPLENISH_MS: u64 = 200;

/// Burst allowance for the per-IP limiter. Lets a caller send a brief
/// spike without immediately tripping the limit.
const DIPS_PER_IP_BURST: u32 = 10;

/// Channel depth of the outer Buffer wrapper. The wrapper makes the layer
/// chain Clone-able so tonic's `Server::layer` accepts it; the actual
/// timeout/rate-limit/per-IP layers run inside the buffered task. Requests
/// beyond this depth are rejected with `BufferError` until earlier ones
/// drain. Sized comfortably above the global rate-limit-per-second so a
/// healthy burst never bumps the channel.
const DIPS_BUFFER_DEPTH: usize = 1024;

/// Key extractor for tonic that reads the peer IP from `TcpConnectInfo`,
/// which the tonic server adds to request extensions for non-TLS TCP
/// connections. The `tower_governor` defaults look for axum's
/// `ConnectInfo<SocketAddr>` extension instead, which tonic does not add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TonicPeerIpKeyExtractor;

impl KeyExtractor for TonicPeerIpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &axum::http::Request<T>) -> Result<Self::Key, GovernorError> {
        req.extensions()
            .get::<TcpConnectInfo>()
            .and_then(|ci| ci.remote_addr())
            .map(|addr| addr.ip())
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

async fn start_dips_server(
    addr: SocketAddr,
    service: impl IndexerDipsService,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let per_ip_config = Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(DIPS_PER_IP_REPLENISH_MS)
            .burst_size(DIPS_PER_IP_BURST)
            .key_extractor(TonicPeerIpKeyExtractor)
            .finish()
            .expect("per-IP governor config invariants"),
    );
    let per_ip_layer = GovernorLayer {
        config: per_ip_config,
    };

    let layer = ServiceBuilder::new()
        .layer(GrpcErrorToResponseLayer)
        .buffer(DIPS_BUFFER_DEPTH)
        .timeout(DIPS_REQUEST_TIMEOUT)
        .layer(per_ip_layer)
        .rate_limit(DIPS_RATE_LIMIT_PER_SEC, Duration::from_secs(1))
        // tonic's Routes returns Response<tonic::body::Body>, but tower_governor
        // hardcodes Response<axum::body::Body>. Convert the inner body before
        // the rate-limit layers see it. tonic's Server is happy to serve any
        // http_body::Body for the response, so axum's body works end-to-end.
        .map_response(|res: axum::http::Response<tonic::body::Body>| res.map(axum::body::Body::new))
        .into_inner();

    if let Err(e) = tonic::transport::Server::builder()
        .layer(layer)
        .add_service(IndexerDipsServiceServer::new(service))
        .serve_with_shutdown(addr, shutdown)
        .await
    {
        tracing::error!(error = %e, "DIPS gRPC server error");
    }
}

async fn create_subgraph_client(
    http_client: reqwest::Client,
    graph_node: &GraphNodeConfig,
    subgraph_config: &SubgraphConfig,
) -> &'static SubgraphClient {
    Box::leak(Box::new(
        SubgraphClient::new(
            http_client,
            subgraph_config.deployment_id.map(|deployment| {
                DeploymentDetails::for_graph_node_url(
                    graph_node.status_url.clone(),
                    graph_node.query_url.clone(),
                    deployment,
                )
            }),
            DeploymentDetails::for_query_url_with_token(
                subgraph_config.query_url.clone(),
                subgraph_config.query_auth_token.clone(),
            ),
        )
        .await,
    ))
}

/// Creates an HTTP client with the specified timeout configuration.
///
/// # Arguments
/// * `timeout` - Maximum duration to wait for a response
/// * `tcp_nodelay` - If true, disables Nagle's algorithm for lower latency
fn create_http_client(
    timeout: Duration,
    tcp_nodelay: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .tcp_nodelay(tcp_nodelay)
        .timeout(timeout)
        .build()
}

/// Graceful shutdown handler that coordinates shutdown across all servers
async fn shutdown_handler(shutdown_token: CancellationToken) {
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

    tracing::info!("Signal received, starting graceful shutdown");
    shutdown_token.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_grt_zero() {
        // Arrange
        let wei = 0u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0");
    }

    #[test]
    fn test_format_grt_whole_number() {
        // Arrange - 1 GRT = 10^18 wei
        let wei = 1_000_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1");
    }

    #[test]
    fn test_format_grt_large_whole_number() {
        // Arrange - 1000 GRT
        let wei = 1_000_000_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1000");
    }

    #[test]
    fn test_format_grt_small_value_less_than_one() {
        // Arrange - 0.5 GRT = 5 * 10^17 wei
        let wei = 500_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.5");
    }

    #[test]
    fn test_format_grt_very_small_value() {
        // Arrange - 0.000000000000000001 GRT = 1 wei
        let wei = 1u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.000000000000000001");
    }

    #[test]
    fn test_format_grt_mixed_value() {
        // Arrange - 1.5 GRT
        let wei = 1_500_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1.5");
    }

    #[test]
    fn test_format_grt_trims_trailing_zeros() {
        // Arrange - 1.100 GRT should become "1.1"
        let wei = 1_100_000_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "1.1");
    }

    #[test]
    fn test_format_grt_many_decimal_places() {
        // Arrange - 0.123456789012345678 GRT
        let wei = 123_456_789_012_345_678u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "0.123456789012345678");
    }

    #[test]
    fn test_format_grt_large_value_with_decimals() {
        // Arrange - 12345.6789 GRT
        let wei = 12_345_678_900_000_000_000_000u128;

        // Act
        let result = format_grt(wei);

        // Assert
        assert_eq!(result, "12345.6789");
    }
}
