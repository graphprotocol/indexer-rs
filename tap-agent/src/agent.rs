// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use alloy_sol_types::eip712_domain;
use indexer_common::prelude::{
    escrow_accounts, indexer_allocations, DeploymentDetails, SubgraphClient,
};

use crate::{
    aggregator_endpoints, config, database,
    tap::sender_allocation_relationships_manager::SenderAllocationRelationshipsManager,
};

pub async fn start_agent(config: &'static config::Cli) {
    let pgpool = database::connect(&config.postgres).await;

    let http_client = reqwest::Client::new();

    let network_subgraph = Box::leak(Box::new(SubgraphClient::new(
        http_client.clone(),
        config
            .network_subgraph
            .network_subgraph_deployment
            .map(|deployment| {
                DeploymentDetails::for_graph_node(
                    &config.indexer_infrastructure.graph_node_status_endpoint,
                    &config.indexer_infrastructure.graph_node_query_endpoint,
                    deployment,
                )
            })
            .transpose()
            .expect("Failed to parse graph node query endpoint and network subgraph deployment"),
        DeploymentDetails::for_query_url(&config.network_subgraph.network_subgraph_endpoint)
            .expect("Failed to parse network subgraph endpoint"),
    )));

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        config.ethereum.indexer_address,
        1,
        Duration::from_secs(config.network_subgraph.allocation_syncing_interval),
    );

    let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
        http_client.clone(),
        config
            .escrow_subgraph
            .escrow_subgraph_deployment
            .map(|deployment| {
                DeploymentDetails::for_graph_node(
                    &config.indexer_infrastructure.graph_node_status_endpoint,
                    &config.indexer_infrastructure.graph_node_query_endpoint,
                    deployment,
                )
            })
            .transpose()
            .expect("Failed to parse graph node query endpoint and escrow subgraph deployment"),
        DeploymentDetails::for_query_url(&config.escrow_subgraph.escrow_subgraph_endpoint)
            .expect("Failed to parse escrow subgraph endpoint"),
    )));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        config.ethereum.indexer_address,
        Duration::from_secs(config.escrow_subgraph.escrow_syncing_interval),
    );

    // TODO: replace with a proper implementation once the gateway registry contract is ready
    let sender_aggregator_endpoints = aggregator_endpoints::load_aggregator_endpoints(
        config.tap.sender_aggregator_endpoints_file.clone(),
    );

    let tap_eip712_domain_separator = eip712_domain! {
        name: "Scalar TAP",
        version: "1",
        chain_id: config.receipts.receipts_verifier_chain_id,
        verifying_contract: config.receipts.receipts_verifier_address,
    };

    let _sender_allocation_relationships_manager = SenderAllocationRelationshipsManager::new(
        config,
        pgpool,
        indexer_allocations,
        escrow_accounts,
        escrow_subgraph,
        tap_eip712_domain_separator,
        sender_aggregator_endpoints,
    )
    .await;
}
