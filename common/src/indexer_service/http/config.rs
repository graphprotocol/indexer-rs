// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thegraph::types::Address;
use thegraph::types::DeploymentId;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DatabaseConfig {
    pub postgres_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubgraphConfig {
    #[serde(default)]
    pub serve_subgraph: bool,
    pub serve_auth_token: Option<String>,

    pub deployment: Option<DeploymentId>,
    pub query_url: String,
    pub syncing_interval: u64,
    #[serde(default = "one_hour")]
    pub recently_closed_allocation_buffer_seconds: u64,
}

fn one_hour() -> u64 {
    3600
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host_and_port: SocketAddr,
    pub metrics_host_and_port: SocketAddr,
    pub url_prefix: String,
    pub free_query_auth_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexerServiceConfig {
    pub indexer: IndexerConfig,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub graph_node: Option<GraphNodeConfig>,
    pub network_subgraph: SubgraphConfig,
    pub escrow_subgraph: SubgraphConfig,
    pub graph_network: GraphNetworkConfig,
    pub scalar: ScalarConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphNodeConfig {
    pub status_url: String,
    pub query_base_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphNetworkConfig {
    pub id: u64,
    pub chain_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexerConfig {
    pub indexer_address: Address,
    pub operator_mnemonic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScalarConfig {
    pub chain_id: u64,
    pub receipts_verifier_address: Address,
    pub timestamp_threshold: Duration,
}
