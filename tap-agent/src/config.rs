// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use indexer_config::{Config as IndexerConfig, ConfigPrefix};
use reqwest::Url;
use std::path::PathBuf;
use std::{collections::HashMap, str::FromStr};

use anyhow::Result;
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

impl From<IndexerConfig> for Config {
    fn from(value: IndexerConfig) -> Self {
        Self {
            ethereum: Ethereum {
                indexer_address: value.indexer.indexer_address,
            },
            receipts: Receipts {
                receipts_verifier_chain_id: value.blockchain.chain_id as u64,
                receipts_verifier_address: value.blockchain.receipts_verifier_address,
            },
            indexer_infrastructure: IndexerInfrastructure {
                metrics_port: value.metrics.port,
                graph_node_query_endpoint: value.graph_node.query_url.into(),
                graph_node_status_endpoint: value.graph_node.status_url.into(),
                log_level: None,
            },
            postgres: Postgres {
                postgres_url: value.database.postgres_url,
            },
            network_subgraph: NetworkSubgraph {
                network_subgraph_deployment: value.subgraphs.network.config.deployment_id,
                network_subgraph_endpoint: value.subgraphs.network.config.query_url.into(),
                network_subgraph_auth_token: value.subgraphs.network.config.query_auth_token,
                allocation_syncing_interval_ms: value
                    .subgraphs
                    .network
                    .config
                    .syncing_interval_secs
                    .as_millis() as u64,
                recently_closed_allocation_buffer_seconds: value
                    .subgraphs
                    .network
                    .recently_closed_allocation_buffer_secs
                    .as_secs(),
            },
            escrow_subgraph: EscrowSubgraph {
                escrow_subgraph_deployment: value.subgraphs.escrow.config.deployment_id,
                escrow_subgraph_endpoint: value.subgraphs.escrow.config.query_url.into(),
                escrow_subgraph_auth_token: value.subgraphs.escrow.config.query_auth_token,
                escrow_syncing_interval_ms: value
                    .subgraphs
                    .escrow
                    .config
                    .syncing_interval_secs
                    .as_millis() as u64,
            },
            tap: Tap {
                rav_request_trigger_value: value.tap.get_trigger_value(),
                rav_request_timestamp_buffer_ms: value
                    .tap
                    .rav_request
                    .timestamp_buffer_secs
                    .as_millis() as u64,
                rav_request_timeout_secs: value.tap.rav_request.request_timeout_secs.as_secs(),
                sender_aggregator_endpoints: value
                    .tap
                    .sender_aggregator_endpoints
                    .into_iter()
                    .map(|(addr, url)| (addr, url.into()))
                    .collect(),
                rav_request_receipt_limit: value.tap.rav_request.max_receipts_per_request,
                max_unnaggregated_fees_per_sender: value
                    .tap
                    .max_amount_willing_to_lose_grt
                    .get_value(),
            },
            config: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
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

#[derive(Clone, Debug, Default)]
pub struct Ethereum {
    pub indexer_address: Address,
}

#[derive(Clone, Debug, Default)]
pub struct Receipts {
    pub receipts_verifier_chain_id: u64,
    pub receipts_verifier_address: Address,
}

#[derive(Clone, Debug, Default)]
pub struct IndexerInfrastructure {
    pub metrics_port: u16,
    pub graph_node_query_endpoint: String,
    pub graph_node_status_endpoint: String,
    pub log_level: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Postgres {
    pub postgres_url: Url,
}

impl Default for Postgres {
    fn default() -> Self {
        Self {
            postgres_url: Url::from_str("postgres:://postgres@postgres/postgres").unwrap(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSubgraph {
    pub network_subgraph_deployment: Option<DeploymentId>,
    pub network_subgraph_endpoint: String,
    pub network_subgraph_auth_token: Option<String>,
    pub allocation_syncing_interval_ms: u64,
    pub recently_closed_allocation_buffer_seconds: u64,
}

#[derive(Clone, Debug, Default)]
pub struct EscrowSubgraph {
    pub escrow_subgraph_deployment: Option<DeploymentId>,
    pub escrow_subgraph_endpoint: String,
    pub escrow_subgraph_auth_token: Option<String>,
    pub escrow_syncing_interval_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Tap {
    pub rav_request_trigger_value: u128,
    pub rav_request_timestamp_buffer_ms: u64,
    pub rav_request_timeout_secs: u64,
    pub sender_aggregator_endpoints: HashMap<Address, String>,
    pub rav_request_receipt_limit: u64,
    pub max_unnaggregated_fees_per_sender: u128,
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
        let indexer_config =
            IndexerConfig::parse(ConfigPrefix::Tap, &cli.config).map_err(|e| anyhow::anyhow!(e))?;
        let config: Config = indexer_config.into();

        // Enables tracing under RUST_LOG variable
        if let Some(log_setting) = &config.indexer_infrastructure.log_level {
            std::env::set_var("RUST_LOG", log_setting);
        };

        // add a LogFormat to config
        init_tracing("pretty".to_string()).expect(
            "Could not set up global default subscriber for logger, check \
        environmental variable `RUST_LOG`",
        );

        Ok(config)
    }
}
