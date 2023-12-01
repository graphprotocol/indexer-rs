// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{path::PathBuf, str::FromStr};

use alloy_primitives::Address;
use bigdecimal::{BigDecimal, ToPrimitive};
use clap::{command, Args, Parser, ValueEnum};
use dotenvy::dotenv;
use serde::{Deserialize, Serialize};
use thegraph::types::DeploymentId;
use tracing::subscriber::{set_global_default, SetGlobalDefaultError};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Clone, Debug, Parser, Serialize, Deserialize, Default)]
#[clap(
    name = "indexer-tap-agent",
    about = "Agent that manages Timeline Aggregation Protocol (TAP) receipts as well \
    as Receipt Aggregate Voucher (RAV) requests for an indexer."
)]
#[command(author, version, about, long_about = None, arg_required_else_help = true)]
pub struct Cli {
    #[command(flatten)]
    pub ethereum: Ethereum,
    #[command(flatten)]
    pub receipts: Receipts,
    #[command(flatten)]
    pub indexer_infrastructure: IndexerInfrastructure,
    #[command(flatten)]
    pub postgres: Postgres,
    #[command(flatten)]
    pub network_subgraph: NetworkSubgraph,
    #[command(flatten)]
    pub escrow_subgraph: EscrowSubgraph,
    #[command(flatten)]
    pub tap: Tap,
    #[arg(
        short,
        value_name = "config",
        env = "CONFIG",
        help = "Indexer service configuration file (YAML format)"
    )]
    pub config: Option<String>,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(required = true, multiple = true)]
pub struct Ethereum {
    #[clap(
        long,
        value_name = "indexer-address",
        env = "INDEXER_ADDRESS",
        help = "Ethereum address of the indexer"
    )]
    pub indexer_address: Address,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(required = true, multiple = true)]
pub struct Receipts {
    #[clap(
        long,
        value_name = "receipts-verifier-chain-id",
        env = "RECEIPTS_VERIFIER_CHAIN_ID",
        help = "Scalar TAP verifier chain ID"
    )]
    pub receipts_verifier_chain_id: u64,
    #[clap(
        long,
        value_name = "receipts-verifier-address",
        env = "RECEIPTS_VERIFIER_ADDRESS",
        help = "Scalar TAP verifier contract address"
    )]
    pub receipts_verifier_address: Address,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(multiple = true)]
pub struct IndexerInfrastructure {
    #[clap(
        long,
        value_name = "metrics-port",
        env = "METRICS_PORT",
        default_value_t = 7300,
        help = "Port to serve Prometheus metrics at"
    )]
    pub metrics_port: u16,
    #[clap(
        long,
        value_name = "graph-node-query-endpoint",
        env = "GRAPH_NODE_QUERY_ENDPOINT",
        default_value_t = String::from("http://0.0.0.0:8000"),
        help = "Graph node GraphQL HTTP service endpoint",
    )]
    pub graph_node_query_endpoint: String,
    #[clap(
        long,
        value_name = "graph-node-status-endpoint",
        env = "GRAPH_NODE_STATUS_ENDPOINT",
        default_value_t = String::from("http://0.0.0.0:8030"),
        help = "Graph node endpoint for the index node server",
    )]
    pub graph_node_status_endpoint: String,
    #[clap(
        long,
        value_name = "log-level",
        env = "LOG_LEVEL",
        value_enum,
        help = "Log level in RUST_LOG format"
    )]
    pub log_level: Option<String>,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(required = true, multiple = true)]
pub struct Postgres {
    #[clap(
        long,
        value_name = "postgres-host",
        env = "POSTGRES_HOST",
        default_value_t = String::from("http://0.0.0.0/"),
        help = "Postgres host",
    )]
    pub postgres_host: String,
    #[clap(
        long,
        value_name = "postgres-port",
        env = "POSTGRES_PORT",
        default_value_t = 5432,
        help = "Postgres port"
    )]
    pub postgres_port: usize,
    #[clap(
        long,
        value_name = "postgres-database",
        env = "POSTGRES_DATABASE",
        help = "Postgres database name"
    )]
    pub postgres_database: String,
    #[clap(
        long,
        value_name = "postgres-username",
        env = "POSTGRES_USERNAME",
        default_value_t = String::from("postgres"),
        help = "Postgres username",
    )]
    pub postgres_username: String,
    #[clap(
        long,
        value_name = "postgres-password",
        env = "POSTGRES_PASSWORD",
        default_value_t = String::from(""),
        help = "Postgres password",
    )]
    pub postgres_password: String,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(multiple = true)]
pub struct NetworkSubgraph {
    #[clap(
        long,
        value_name = "network-subgraph-deployment",
        env = "NETWORK_SUBGRAPH_DEPLOYMENT",
        help = "Network subgraph deployment"
    )]
    pub network_subgraph_deployment: Option<DeploymentId>,
    #[clap(
        long,
        value_name = "network-subgraph-endpoint",
        env = "NETWORK_SUBGRAPH_ENDPOINT",
        default_value_t = String::from("https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli"),
        help = "Endpoint to query the network subgraph from",
    )]
    pub network_subgraph_endpoint: String,
    #[clap(
        long,
        value_name = "allocation-syncing-interval",
        env = "ALLOCATION_SYNCING_INTERVAL",
        default_value_t = 120_000,
        help = "Interval (in ms) for syncing indexer allocations from the network"
    )]
    pub allocation_syncing_interval_ms: u64,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(required = true, multiple = true)]
pub struct EscrowSubgraph {
    #[clap(
        long,
        value_name = "escrow-subgraph-deployment",
        env = "ESCROW_SUBGRAPH_DEPLOYMENT",
        help = "Escrow subgraph deployment"
    )]
    pub escrow_subgraph_deployment: Option<DeploymentId>,
    #[clap(
        long,
        value_name = "escrow-subgraph-endpoint",
        env = "ESCROW_SUBGRAPH_ENDPOINT",
        // TODO:
        // default_value_t = String::from("https://api.thegraph.com/subgraphs/name/?????????????"),
        help = "Endpoint to query the network subgraph from"
    )]
    pub escrow_subgraph_endpoint: String,
    #[clap(
        long,
        value_name = "escrow-syncing-interval",
        env = "ESCROW_SYNCING_INTERVAL",
        default_value_t = 120_000,
        help = "Interval (in ms) for syncing indexer escrow accounts from the escrow subgraph"
    )]
    pub escrow_syncing_interval_ms: u64,
}

#[derive(Clone, Debug, Args, Serialize, Deserialize, Default)]
#[group(required = true, multiple = true)]
pub struct Tap {
    #[clap(
        long,
        value_name = "rav-request-trigger-value",
        env = "RAV_REQUEST_TRIGGER_VALUE",
        help = "Value of unaggregated fees that triggers a RAV request (in GRT).",
        default_value = "10",
        value_parser(parse_grt_value_to_nonzero_u64)
    )]
    pub rav_request_trigger_value: u64,
    #[clap(
        long,
        value_name = "rav-request-timestamp-buffer",
        env = "RAV_REQUEST_TIMESTAMP_BUFFER",
        help = "Buffer (in ms) to add between the current time and the timestamp of the \
        last unaggregated fee when triggering a RAV request.",
        default_value_t = 1_000 // 1 second
    )]
    pub rav_request_timestamp_buffer_ms: u64,
    #[clap(
        long,
        value_name = "rav-request-timeout",
        env = "RAV_REQUEST_TIMEOUT",
        help = "Timeout (in seconds) for RAV requests.",
        default_value_t = 5
    )]
    pub rav_request_timeout_secs: u64,

    // TODO: Remove this whenever the the gateway registry is ready
    #[clap(
        long,
        value_name = "sender-aggregator-endpoints",
        env = "SENDER_AGGREGATOR_ENDPOINTS",
        help = "YAML file with a map of sender addresses to aggregator endpoints."
    )]
    pub sender_aggregator_endpoints_file: PathBuf,
}

/// Sets up tracing, allows log level to be set from the environment variables
fn init_tracing(format: String) -> Result<(), SetGlobalDefaultError> {
    let filter = EnvFilter::from_default_env();
    let subscriber_builder: tracing_subscriber::fmt::SubscriberBuilder<
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format,
        EnvFilter,
    > = FmtSubscriber::builder().with_env_filter(filter);
    match format.as_str() {
        "json" => set_global_default(subscriber_builder.json().finish()),
        "full" => set_global_default(subscriber_builder.finish()),
        "compact" => set_global_default(subscriber_builder.compact().finish()),
        _ => set_global_default(subscriber_builder.with_ansi(true).pretty().finish()),
    }
}

fn parse_grt_value_to_nonzero_u64(s: &str) -> Result<u64, std::io::Error> {
    let v = BigDecimal::from_str(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    if v <= 0.into() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "GRT value must be greater than 0".to_string(),
        ));
    }
    // Convert to wei
    let v = v * BigDecimal::from(10u64.pow(18));
    // Convert to u64
    v.to_u64().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "GRT value cannot be represented as a u64 GRT wei value".to_string(),
        )
    })
}

impl Cli {
    /// Parse config arguments If environmental variable for config is set to a valid
    /// config file path, then parse from config Otherwise parse from command line
    /// arguments
    pub fn args() -> Self {
        dotenv().ok();
        let cli = if let Ok(file_path) = std::env::var("config") {
            confy::load_path::<Cli>(file_path.clone())
                .unwrap_or_else(|_| panic!("Parse config file at {}", file_path.clone()))
        } else {
            Cli::parse()
            // Potentially store it for the user let _ = confy::store_path("./args.toml",
            // cli.clone());
        };

        // Enables tracing under RUST_LOG variable
        if let Some(log_setting) = &cli.indexer_infrastructure.log_level {
            std::env::set_var("RUST_LOG", log_setting);
        };

        // add a LogFormat to config
        init_tracing("pretty".to_string()).expect(
            "Could not set up global default subscriber for logger, check \
        environmental variable `RUST_LOG` or the CLI input `log-level`",
        );
        cli
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Validate the input: {0}")]
    ValidateInput(String),
    #[error("Generate JSON representation of the config file: {0}")]
    GenerateJson(serde_json::Error),
    #[error("Toml file error: {0}")]
    ReadStr(std::io::Error),
    #[error("Unknown error: {0}")]
    Other(anyhow::Error),
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize, Deserialize, Default,
)]
pub enum LogLevel {
    Trace,
    #[default]
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_grt_value_to_u64() {
        assert_eq!(
            parse_grt_value_to_nonzero_u64("1").unwrap(),
            1_000_000_000_000_000_000
        );
        assert_eq!(
            parse_grt_value_to_nonzero_u64("1.1").unwrap(),
            1_100_000_000_000_000_000
        );
        assert_eq!(
            parse_grt_value_to_nonzero_u64("1.000000000000000001").unwrap(),
            1_000_000_000_000_000_001
        );
        assert_eq!(
            parse_grt_value_to_nonzero_u64("0.000000000000000001").unwrap(),
            1
        );
        assert!(parse_grt_value_to_nonzero_u64("0").is_err());
        assert!(parse_grt_value_to_nonzero_u64("-1").is_err());
        assert_eq!(
            parse_grt_value_to_nonzero_u64("1.0000000000000000001").unwrap(),
            1_000_000_000_000_000_000
        );
    }
}
