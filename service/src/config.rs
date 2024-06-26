// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use indexer_common::indexer_service::http::{
    DatabaseConfig, GraphNetworkConfig, GraphNodeConfig, IndexerConfig, IndexerServiceConfig,
    ServerConfig, SubgraphConfig, TapConfig,
};
use indexer_config::Config as MainConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config(pub IndexerServiceConfig);

impl From<MainConfig> for Config {
    fn from(value: MainConfig) -> Self {
        Self(IndexerServiceConfig {
            indexer: IndexerConfig {
                indexer_address: value.indexer.indexer_address,
                operator_mnemonic: value.indexer.operator_mnemonic.to_string(),
            },
            server: ServerConfig {
                host_and_port: value.service.host_and_port,
                metrics_host_and_port: SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::new(0, 0, 0, 0),
                    value.metrics.port,
                )),
                url_prefix: value.service.url_prefix,
                free_query_auth_token: value.service.free_query_auth_token,
            },
            database: DatabaseConfig {
                postgres_url: value.database.postgres_url.into(),
            },
            graph_node: Some(GraphNodeConfig {
                status_url: value.graph_node.status_url.into(),
                query_base_url: value.graph_node.query_url.into(),
            }),
            network_subgraph: SubgraphConfig {
                serve_subgraph: value.service.serve_network_subgraph,
                serve_auth_token: value.service.serve_auth_token.clone(),
                deployment: value.subgraphs.network.config.deployment_id,
                query_url: value.subgraphs.network.config.query_url.into(),
                query_auth_token: value.subgraphs.network.config.query_auth_token.clone(),
                syncing_interval: value
                    .subgraphs
                    .network
                    .config
                    .syncing_interval_secs
                    .as_secs(),
                recently_closed_allocation_buffer_seconds: value
                    .subgraphs
                    .network
                    .recently_closed_allocation_buffer_secs
                    .as_secs(),
            },
            escrow_subgraph: SubgraphConfig {
                serve_subgraph: value.service.serve_escrow_subgraph,
                serve_auth_token: value.service.serve_auth_token,
                deployment: value.subgraphs.escrow.config.deployment_id,
                query_url: value.subgraphs.escrow.config.query_url.into(),
                query_auth_token: value.subgraphs.network.config.query_auth_token.clone(),
                syncing_interval: value
                    .subgraphs
                    .escrow
                    .config
                    .syncing_interval_secs
                    .as_secs(),
                recently_closed_allocation_buffer_seconds: 0,
            },
            graph_network: GraphNetworkConfig {
                chain_id: value.blockchain.chain_id.clone() as u64,
            },
            tap: TapConfig {
                chain_id: value.blockchain.chain_id as u64,
                receipts_verifier_address: value.blockchain.receipts_verifier_address,
                timestamp_error_tolerance: value.tap.rav_request.timestamp_buffer_secs.as_secs(),
                receipt_max_value: value.service.tap.max_receipt_value_grt.get_value() as u64,
            },
        })
    }
}
