// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use thegraph::types::Address;
use thegraph::types::DeploymentId;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DatabaseConfig {
    pub postgres_url: String,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubgraphConfig {
    #[serde(default)]
    pub serve_subgraph: bool,
    pub serve_auth_token: Option<String>,

    pub deployment: Option<DeploymentId>,
    pub query_url: String,
    #[serde_inline_default(60)]
    pub syncing_interval: u64,
    #[serde_inline_default(3600)]
    pub recently_closed_allocation_buffer_seconds: u64,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde_inline_default("0.0.0.0:7600".parse().unwrap())]
    pub host_and_port: SocketAddr,
    #[serde_inline_default("0.0.0.0:7300".parse().unwrap())]
    pub metrics_host_and_port: SocketAddr,
    #[serde_inline_default("/".to_string())]
    pub url_prefix: String,
    pub free_query_auth_token: Option<String>,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexerServiceConfig {
    pub indexer: IndexerConfig,
    #[serde_inline_default(serde_json::from_str(r#"{}"#).unwrap())] // Allow missing
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

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphNetworkConfig {
    #[serde_inline_default(1)]
    pub id: u64,
    pub chain_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexerConfig {
    pub indexer_address: Address,
    pub operator_mnemonic: String,
}

#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScalarConfig {
    pub chain_id: u64,
    pub receipts_verifier_address: Address,
    #[serde_inline_default(30)]
    pub timestamp_error_tolerance: u64,
    pub receipt_max_value: u64,
}
