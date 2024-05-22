// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use bigdecimal::{BigDecimal, ToPrimitive};
use clap::Parser;

use anyhow::Result;
use figment::providers::{Format, Toml};
use figment::Figment;
use serde::{de, Deserialize, Deserializer};
use thegraph::types::{Address, DeploymentId};
use tracing::subscriber::{set_global_default, SetGlobalDefaultError};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser)]
pub struct Cli {
    /// Path to the configuration file.
    /// See https://github.com/graphprotocol/indexer-rs/tree/main/tap-agent for examples.
    #[arg(long, value_name = "FILE", verbatim_doc_comment)]
    pub config: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Config {
    pub ethereum: Ethereum,
    pub receipts: Receipts,
    pub indexer_infrastructure: IndexerInfrastructure,
    pub postgres: Postgres,
    pub network_subgraph: NetworkSubgraph,
    pub escrow_subgraph: EscrowSubgraph,
    pub tap: Tap,
    pub config: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Ethereum {
    pub indexer_address: Address,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Receipts {
    pub receipts_verifier_chain_id: u64,
    pub receipts_verifier_address: Address,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct IndexerInfrastructure {
    pub metrics_port: u16,
    pub graph_node_query_endpoint: String,
    pub graph_node_status_endpoint: String,
    pub log_level: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Postgres {
    pub postgres_host: String,
    pub postgres_port: usize,
    pub postgres_database: String,
    pub postgres_username: String,
    pub postgres_password: String,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct NetworkSubgraph {
    pub network_subgraph_deployment: Option<DeploymentId>,
    pub network_subgraph_endpoint: String,
    pub allocation_syncing_interval_ms: u64,
    pub recently_closed_allocation_buffer_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct EscrowSubgraph {
    pub escrow_subgraph_deployment: Option<DeploymentId>,
    pub escrow_subgraph_endpoint: String,
    pub escrow_syncing_interval_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Tap {
    #[serde(deserialize_with = "parse_grt_value_to_nonzero_u128")]
    pub rav_request_trigger_value: u128,
    pub rav_request_timestamp_buffer_ms: u64,
    pub rav_request_timeout_secs: u64,
    pub sender_aggregator_endpoints_file: PathBuf,
    pub rav_request_receipt_limit: u64,
    #[serde(deserialize_with = "parse_grt_value_to_nonzero_u128")]
    pub max_unnaggregated_fees_per_sender: u128,
}

fn parse_grt_value_to_nonzero_u128<'de, D>(deserializer: D) -> Result<u128, D::Error>
where
    D: Deserializer<'de>,
{
    let v = BigDecimal::deserialize(deserializer)?;
    if v <= 0.into() {
        return Err(de::Error::custom("GRT value must be greater than 0"));
    }
    // Convert to wei
    let v = v * BigDecimal::from(10u64.pow(18));
    // Convert to u128
    v.to_u128()
        .ok_or_else(|| de::Error::custom("GRT value cannot be represented as a u128 GRT wei value"))
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

impl Config {
    pub fn from_cli() -> Result<Self> {
        let cli = Cli::parse();
        let config = Config::load(&cli.config)?;

        // Enables tracing under RUST_LOG variable
        if let Some(log_setting) = &config.indexer_infrastructure.log_level {
            std::env::set_var("RUST_LOG", log_setting);
        };

        // add a LogFormat to config
        init_tracing("pretty".to_string()).expect(
            "Could not set up global default subscriber for logger, check \
        environmental variable `RUST_LOG` or the CLI input `log-level`",
        );

        Ok(config)
    }

    pub fn load(filename: &PathBuf) -> Result<Self> {
        let config_defaults: &str = r##"
            [indexer_infrastructure]
            metrics_port = 7300
            log_level = "info"
            
            [postgres]
            postgres_port = 5432
            
            [network_subgraph]
            allocation_syncing_interval_ms = 60000
            recently_closed_allocation_buffer_seconds = 3600
            
            [escrow_subgraph]
            escrow_syncing_interval_ms = 60000
            
            [tap]
            rav_request_trigger_value = 10
            rav_request_timestamp_buffer_ms = 60000
            rav_request_timeout_secs = 5
            rav_request_receipt_limit = 10000
            max_unnaggregated_fees_per_sender = 20
        "##;

        // Load the user config file
        let config_str = std::fs::read_to_string(filename)?;

        // Remove TOML comments, so that we can have shell expansion examples in the file.
        let config_str = config_str
            .lines()
            .filter(|line| !line.trim().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        let config_str = shellexpand::env(&config_str)?;

        let config: Config = Figment::new()
            .merge(Toml::string(config_defaults))
            .merge(Toml::string(&config_str))
            .extract()?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_assert::{Deserializer, Token};

    use super::*;

    #[test]
    fn test_parse_grt_value_to_u128_deserialize() {
        macro_rules! parse {
            ($input:expr) => {{
                let mut deserializer =
                    Deserializer::builder([Token::Str($input.to_string())]).build();
                parse_grt_value_to_nonzero_u128(&mut deserializer)
            }};
        }

        assert_eq!(parse!("1").unwrap(), 1_000_000_000_000_000_000);
        assert_eq!(parse!("1.1").unwrap(), 1_100_000_000_000_000_000);
        assert_eq!(
            parse!("1.000000000000000001").unwrap(),
            1_000_000_000_000_000_001
        );
        assert_eq!(parse!("0.000000000000000001").unwrap(), 1);
        assert_eq!(
            parse!("0").unwrap_err().to_string(),
            "GRT value must be greater than 0"
        );
        assert_eq!(
            parse!("-1").unwrap_err().to_string(),
            "GRT value must be greater than 0"
        );
        assert_eq!(
            parse!("1.0000000000000000001").unwrap(),
            1_000_000_000_000_000_000
        );
        assert_eq!(
            parse!(format!("{}0", u128::MAX)).unwrap_err().to_string(),
            "GRT value cannot be represented as a u128 GRT wei value"
        );
    }

    /// Test loading the minimal configuration example file.
    /// Makes sure that the minimal template is up to date with the code.
    /// Note that it doesn't check that the config is actually minimal, but rather that all missing
    /// fields have defaults. The burden of making sure the config is minimal is on the developer.
    #[test]
    fn test_minimal_config() {
        Config::load(&PathBuf::from("minimal-config-example.toml")).unwrap();
    }

    /// Test that the maximal configuration file is up to date with the code.
    /// Make sure that `test_minimal_config` passes before looking at this.
    #[test]
    fn test_maximal_config() {
        // Generate full config by deserializing the minimal config and let the code fill in the defaults.
        let max_config = Config::load(&PathBuf::from("minimal-config-example.toml")).unwrap();
        let max_config_file: Config = toml::from_str(
            fs::read_to_string("maximal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();

        assert_eq!(max_config, max_config_file);
    }
}
