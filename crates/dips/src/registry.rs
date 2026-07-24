// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Test helpers for network registry validation.
//!
//! The Graph maintains a registry of supported networks. During RCA validation,
//! we check that the subgraph's network is in this registry (or in the indexer's
//! `additional_networks` config for custom/test networks).
//!
//! This module provides [`test_registry`] which returns a minimal registry
//! containing "mainnet" and "hardhat" for use in unit tests.

use graph_networks_registry::NetworksRegistry;

pub fn test_registry() -> NetworksRegistry {
    use graph_networks_registry::{Network, NetworkType, Services};

    NetworksRegistry {
        schema: "".to_string(),
        description: "".to_string(),
        networks: vec![
            Network {
                aliases: None,
                api_urls: None,
                caip2_id: "eip155:1".to_string(),
                docs_url: None,
                explorer_urls: None,
                firehose: None,
                full_name: "".to_string(),
                genesis: None,
                graph_node: None,
                icon: None,
                id: "mainnet".to_string(),
                indexer_docs_urls: None,
                issuance_rewards: false,
                native_token: None,
                network_type: NetworkType::Mainnet,
                relations: None,
                rpc_urls: None,
                second_name: None,
                services: Services {
                    firehose: None,
                    sps: None,
                    subgraphs: None,
                    substreams: None,
                },
                short_name: "mainnet".to_string(),
            },
            Network {
                aliases: None,
                api_urls: None,
                caip2_id: "eip155:1337".to_string(),
                docs_url: None,
                explorer_urls: None,
                firehose: None,
                full_name: "".to_string(),
                genesis: None,
                graph_node: None,
                icon: None,
                id: "hardhat".to_string(),
                indexer_docs_urls: None,
                issuance_rewards: false,
                native_token: None,
                network_type: NetworkType::Mainnet,
                relations: None,
                rpc_urls: None,
                second_name: None,
                services: Services {
                    firehose: None,
                    sps: None,
                    subgraphs: None,
                    substreams: None,
                },
                short_name: "hardhat".to_string(),
            },
        ],
        title: "".to_string(),
        updated_at: "".to_string(),
        version: "".to_string(),
    }
}
