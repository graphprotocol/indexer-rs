// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use thegraph_core::{Address, DeploymentId};
use tracing::warn;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DatabaseConfig {
    pub postgres_url: String,
}
impl DatabaseConfig {
    pub fn format_db_config(
        ps_url: Option<String>,
        ps_host: Option<String>,
        ps_pwd: Option<String>,
        ps_port: Option<String>,
        ps_user: Option<String>,
        ps_db: Option<String>,
    ) -> Self {
        let db_config = (ps_url, ps_host, ps_pwd, ps_port, ps_user, ps_db);
        match db_config {
            (Some(url), ..) if !url.is_empty() => DatabaseConfig { postgres_url: url },
            (None, Some(host), Some(pwd), Some(port), Some(user), Some(dbname)) => {
                let postgres_url =
                    format!("postgres://{}:{}@{}:{}/{}", user, pwd, host, port, dbname);
                DatabaseConfig { postgres_url }
            }
            _ => {
                warn!(
                    "Some configuration values are missing for database values, please make sure you either \
                    pass `postgres_url` or pass all the variables to connect to the database
                ");
                // This will eventually fail to connect
                DatabaseConfig {
                    postgres_url: String::new(),
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubgraphConfig {
    #[serde(default)]
    pub serve_subgraph: bool,
    pub serve_auth_token: Option<String>,

    pub deployment: Option<DeploymentId>,
    pub query_url: String,
    pub query_auth_token: Option<String>,
    pub syncing_interval: u64,
    pub recently_closed_allocation_buffer_seconds: u64,
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
    pub tap: TapConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphNodeConfig {
    pub status_url: String,
    pub query_base_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphNetworkConfig {
    pub chain_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexerConfig {
    pub indexer_address: Address,
    pub operator_mnemonic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TapConfig {
    pub chain_id: u64,
    pub receipts_verifier_address: Address,
    pub timestamp_error_tolerance: u64,
    pub receipt_max_value: u128,
}
