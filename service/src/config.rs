// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use alloy_primitives::Address;
use figment::{
    providers::{Format, Toml},
    Figment,
};
use indexer_common::indexer_service::http::IndexerServiceConfig;
use serde::{Deserialize, Serialize};
use thegraph::types::DeploymentId;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    // pub receipts: Receipts,
    // pub indexer_infrastructure: IndexerInfrastructure,
    // pub postgres: Postgres,
    // pub network_subgraph: NetworkSubgraph,
    // pub escrow_subgraph: EscrowSubgraph,
    pub common: IndexerServiceConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct NetworkSubgraph {
    // #[clap(
    //     long,
    //     value_name = "serve-network-subgraph",
    //     env = "SERVE_NETWORK_SUBGRAPH",
    //     default_value_t = false,
    //     help = "Whether to serve the network subgraph at /network"
    // )]
    pub serve_network_subgraph: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct EscrowSubgraph {
    // #[clap(
    //     long,
    //     value_name = "serve-escrow-subgraph",
    //     env = "SERVE_ESCROW_SUBGRAPH",
    //     default_value_t = false,
    //     help = "Whether to serve the escrow subgraph at /escrow"
    // )]
    // pub serve_escrow_subgraph: bool,
}

impl Config {
    pub fn load(filename: &PathBuf) -> Result<Self, figment::Error> {
        Figment::new().merge(Toml::file(filename)).extract()
    }
}
