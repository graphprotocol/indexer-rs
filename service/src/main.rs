// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use alloy_sol_types::eip712_domain;
use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use axum::Server;
use dotenvy::dotenv;

use ethereum_types::U256;

use std::{net::SocketAddr, str::FromStr};

use tracing::info;

use util::{package_version, shutdown_signal};

use crate::{
    common::network_subgraph::NetworkSubgraph,
    common::{
        database,
        indexer_management_client::{IndexerManagementClient, QueryRoot},
    },
    config::Cli,
    metrics::handle_serve_metrics,
    query_processor::QueryProcessor,
    server::create_server,
    util::public_key,
};

use server::ServerOptions;

mod allocation_monitor;
mod attestation_signers;
mod common;
mod config;
mod escrow_monitor;
mod graph_node;
mod metrics;
mod query_processor;
mod server;
mod tap_manager;
mod util;

#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod test_vectors;

/// Create Indexer service App
///
/// Initialization for server and Query processor
///
/// Validate that graph-node instance is running for Query processor
/// Validate that server is running with a health check
///
/// Parse Requests received
///
/// Route the requests as a FreeQuery
///
/// Return response from Query Processor
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    dotenv().ok();

    // Parse basic configurations
    let config = Cli::args();
    let release = package_version().expect("Failed to resolve for release version");

    // Initialize graph-node client
    let graph_node = graph_node::GraphNodeInstance::new(
        &config.indexer_infrastructure.graph_node_query_endpoint,
    );

    // Make an instance of network subgraph at either
    // graph_node_query_endpoint/subgraphs/id/network_subgraph_deployment
    // or network_subgraph_endpoint
    let network_subgraph = NetworkSubgraph::new(
        Some(&config.indexer_infrastructure.graph_node_query_endpoint),
        config
            .network_subgraph
            .network_subgraph_deployment
            .as_deref(),
        &config.network_subgraph.network_subgraph_endpoint,
    );

    let allocation_monitor = allocation_monitor::AllocationMonitor::new(
        network_subgraph.clone(),
        config.ethereum.indexer_address,
        1,
        config.network_subgraph.allocation_syncing_interval,
    )
    .await
    .expect("Initialize allocation monitor");

    let attestation_signers = attestation_signers::AttestationSigners::new(
        allocation_monitor.clone(),
        config.ethereum.mnemonic.clone(),
        // TODO: Chain ID should be a config
        U256::from(1),
        // TODO: Dispute manager address should be a config
        Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
    );

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema
    // Note: Typically, you'd call `sqlx::migrate!();` here to sync the models
    // which defaults to files in  "./migrations" to sync the database;
    // however, this can cause conflicts with the migrations run by indexer
    // agent. Hence we leave syncing and migrating entirely to the agent and
    // assume the models are up to date in the service.
    let database = database::connect(&config.postgres).await;

    let escrow_monitor = escrow_monitor::EscrowMonitor::new(
        graph_node.clone(),
        config.escrow_subgraph.escrow_subgraph_deployment,
        config.ethereum.indexer_address,
        config.escrow_subgraph.escrow_syncing_interval,
    )
    .await
    .expect("Initialize escrow monitor");

    let tap_manager = tap_manager::TapManager::new(
        database.clone(),
        allocation_monitor.clone(),
        escrow_monitor,
        // TODO: arguments for eip712_domain should be a config
        eip712_domain! {
            name: "TapManager",
            version: "1",
            verifying_contract: config.ethereum.indexer_address,
        },
    );

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor =
        QueryProcessor::new(graph_node.clone(), attestation_signers.clone(), tap_manager);

    // Start indexer service basic metrics
    tokio::spawn(handle_serve_metrics(
        String::from("0.0.0.0"),
        config.indexer_infrastructure.metrics_port,
    ));

    let indexer_management_client = IndexerManagementClient::new(database).await;
    let service_options = ServerOptions::new(
        Some(config.indexer_infrastructure.port),
        release,
        query_processor,
        config.indexer_infrastructure.free_query_auth_token,
        config.indexer_infrastructure.graph_node_status_endpoint,
        indexer_management_client,
        public_key(&config.ethereum.mnemonic).expect("Failed to initiate with operator wallet"),
        network_subgraph,
        config.network_subgraph.network_subgraph_auth_token,
        config.network_subgraph.serve_network_subgraph,
    );

    // defineCostModelModels
    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();

    info!("Initialized server options");
    let app = create_server(service_options, schema).await;

    let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.indexer_infrastructure.port))
        .expect("Start server port");
    info!("Initialized server app at {}", addr);
    Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    Ok(())
}
