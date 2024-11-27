// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_config::{Config, IndexerConfig, SubgraphsConfig, TapConfig};
use indexer_monitor::{create_subgraph_client, escrow_accounts, indexer_allocations};
use ractor::{concurrency::JoinHandle, Actor, ActorRef};
use tap_core::tap_eip712_domain;

use crate::{
    agent::{
        sender_account::SenderAccountConfig,
        sender_accounts_manager::{
            SenderAccountsManager, SenderAccountsManagerArgs, SenderAccountsManagerMessage,
        },
    },
    database,
};

mod metrics;
pub mod sender_account;
pub mod sender_accounts_manager;
pub mod sender_allocation;
pub mod unaggregated_receipts;

pub async fn start_agent(
    config: Config,
) -> (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>) {
    let domain_separator = tap_eip712_domain(
        config.blockchain.chain_id as u64,
        config.blockchain.receipts_verifier_address,
    );
    let Config {
        indexer: IndexerConfig {
            indexer_address, ..
        },
        ref graph_node,
        ref database,
        subgraphs: SubgraphsConfig {
            ref network,
            ref escrow,
        },
        tap:
            TapConfig {
                // TODO: replace with a proper implementation once the gateway registry contract is ready
                ref sender_aggregator_endpoints,
                ..
            },
        ..
    } = config;
    let pgpool = database::connect(database.clone()).await;

    let http_client = reqwest::Client::new();

    let network_subgraph =
        create_subgraph_client(http_client.clone(), graph_node, &network.config).await;

    let indexer_allocations = indexer_allocations(
        network_subgraph,
        indexer_address,
        network.config.syncing_interval_secs,
        network.recently_closed_allocation_buffer_secs,
    )
    .await
    .expect("Failed to initialize indexer_allocations watcher");

    let escrow_subgraph = create_subgraph_client(http_client, graph_node, &escrow.config).await;

    let escrow_accounts = escrow_accounts(
        escrow_subgraph,
        indexer_address,
        escrow.config.syncing_interval_secs,
        false,
    )
    .await
    .expect("Error creating escrow_accounts channel");

    let config = SenderAccountConfig::from_config(&config);

    let args = SenderAccountsManagerArgs::builder()
        .config(config)
        .domain_separator(domain_separator)
        .pgpool(pgpool)
        .indexer_allocations(indexer_allocations)
        .escrow_accounts(escrow_accounts)
        .network_subgraph(network_subgraph)
        .escrow_subgraph(escrow_subgraph)
        .sender_aggregator_endpoints(sender_aggregator_endpoints.clone())
        .build();

    SenderAccountsManager::spawn(None, SenderAccountsManager, args)
        .await
        .expect("Failed to start sender accounts manager actor.")
}
