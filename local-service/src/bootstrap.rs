// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::B256;
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use indexer_common::prelude::{
    Allocation, AllocationStatus, AttestationSigner, SubgraphDeployment,
};
use thegraph_core::{address, Address, DeploymentId};
use tokio::net::TcpListener;
use tracing::info;

use crate::{
    keys::{get_indexer_address_from_toml, get_mnemonic_from_toml, Signer},
    subgraph::{network_subgraph, NETWORK_SUBGRAPH_ROUTE},
};

const HOST: &str = "0.0.0.0";
const PORT: &str = "8000";
pub const CREATED_AT_BLOCK_HASH: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000000";

pub async fn start_server() -> anyhow::Result<()> {
    let signer = Config::signer()?;
    info!("Starting server on {HOST}:{PORT}");
    let port = dotenvy::var("API_PORT").unwrap_or(PORT.into());
    let listener = TcpListener::bind(&format!("{HOST}:{port}")).await?;

    let router = Router::new().route("/health", get(health_check)).route(
        NETWORK_SUBGRAPH_ROUTE,
        post(move || network_subgraph(signer)),
    );

    Ok(axum::serve(listener, router).await?)
}

async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}

struct Config;

impl Config {
    fn signer() -> anyhow::Result<Signer> {
        let mnemonic = get_mnemonic_from_toml("indexer", "operator_mnemonic")?;
        let indexer_address = get_indexer_address_from_toml("indexer", "indexer_address")?;

        let subgraph_deployment_id =
            Self::create_deployment_id("QmUhiH6Z5xo6o3GNzsSvqpGKLmCt6w5A")?;

        let subgraph_deployment = SubgraphDeployment {
            id: DeploymentId::new(B256::from(subgraph_deployment_id)),
            denied_at: None,
        };

        let allocation = Self::create_allocation(subgraph_deployment, indexer_address)?;

        let dispute_address = address!("33f9E93266ce0E108fc85DdE2f71dab555A0F05a");

        let signer = Signer::new(
            AttestationSigner::new(&mnemonic, &allocation, 42161, dispute_address)?,
            allocation,
        );

        Ok(signer)
    }

    fn create_deployment_id(deployment_str: &str) -> anyhow::Result<[u8; 32]> {
        let id: [u8; 32] = deployment_str.as_bytes().try_into()?;
        Ok(id)
    }

    fn create_allocation(
        subgraph_deployment: SubgraphDeployment,
        indexer_address: Address,
    ) -> anyhow::Result<Allocation> {
        Ok(Allocation {
            id: address!("5BcFE6215cbeB2D75cc3b09e01243cd7Ac55B3a7"),
            status: AllocationStatus::Active,
            subgraph_deployment,
            indexer: indexer_address,
            allocated_tokens: Default::default(),
            created_at_epoch: 1,
            created_at_block_hash: CREATED_AT_BLOCK_HASH.into(),
            closed_at_epoch: None,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        })
    }
}
