// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_sol_types::eip712_domain;
use axum::Server;
use dotenvy::dotenv;
use ethereum_types::U256;
use std::{net::SocketAddr, str::FromStr, time::Duration};
use tracing::info;

use indexer_common::{
    prelude::{
        attestation_signers, dispute_manager, escrow_accounts, indexer_allocations, SubgraphClient,
    },
    tap_manager::TapManager,
};

use util::{package_version, shutdown_signal};

use crate::{
    common::database, config::Cli, metrics::handle_serve_metrics, query_processor::QueryProcessor,
    server::create_server, util::public_key,
};

use server::ServerOptions;

mod common;
mod config;
mod graph_node;
mod metrics;
mod query_processor;
mod server;
mod util;

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
    //
    // We're leaking the network subgraph here to obtain a reference with
    // a static lifetime, which avoids having to pass around and clone `Arc`
    // objects everywhere. Since the network subgraph is read-only, this is
    // no problem.
    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            Some(&config.indexer_infrastructure.graph_node_query_endpoint),
            config.network_subgraph.network_subgraph_deployment.as_ref(),
            &config.network_subgraph.network_subgraph_endpoint,
        )
        .expect("Failed to set up network subgraph client"),
    ));

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        config.ethereum.indexer_address,
        1,
        Duration::from_millis(config.network_subgraph.allocation_syncing_interval),
    );

    // TODO: Chain ID should be a config
    let graph_network_id = 1;

    let dispute_manager =
        dispute_manager(network_subgraph, graph_network_id, Duration::from_secs(60));

    let attestation_signers = attestation_signers(
        indexer_allocations.clone(),
        config.ethereum.mnemonic.clone(),
        U256::from(graph_network_id),
        dispute_manager,
    );

    // Establish Database connection necessary for serving indexer management
    // requests with defined schema
    // Note: Typically, you'd call `sqlx::migrate!();` here to sync the models
    // which defaults to files in  "./migrations" to sync the database;
    // however, this can cause conflicts with the migrations run by indexer
    // agent. Hence we leave syncing and migrating entirely to the agent and
    // assume the models are up to date in the service.
    let indexer_management_db = database::connect(&config.postgres).await;

    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            Some(&config.indexer_infrastructure.graph_node_query_endpoint),
            config.escrow_subgraph.escrow_subgraph_deployment.as_ref(),
            &config.escrow_subgraph.escrow_subgraph_endpoint,
        )
        .expect("Failed to set up escrow subgraph client"),
    ));

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        config.ethereum.indexer_address,
        Duration::from_millis(config.escrow_subgraph.escrow_syncing_interval),
    );

    let tap_manager = TapManager::new(
        indexer_management_db.clone(),
        indexer_allocations,
        escrow_accounts,
        // TODO: arguments for eip712_domain should be a config
        eip712_domain! {
            name: "Scalar TAP",
            version: "1",
            chain_id: config.receipts.receipts_verifier_chain_id,
            verifying_contract: config.receipts.receipts_verifier_address,
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

    let service_options = ServerOptions::new(
        Some(config.indexer_infrastructure.port),
        release,
        query_processor,
        config.indexer_infrastructure.free_query_auth_token,
        config.indexer_infrastructure.graph_node_status_endpoint,
        indexer_management_db,
        public_key(&config.ethereum.mnemonic).expect("Failed to initiate with operator wallet"),
        network_subgraph,
        config.network_subgraph.network_subgraph_auth_token,
        config.network_subgraph.serve_network_subgraph,
    );

    info!("Initialized server options");
    let app = create_server(service_options).await;

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
