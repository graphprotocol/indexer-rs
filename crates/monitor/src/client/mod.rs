// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

mod monitor;
mod subgraph_client;

pub use subgraph_client::{DeploymentDetails, SubgraphClient};

use indexer_config::{GraphNodeConfig, SubgraphConfig};

/// Creates a static reference to a subgraph
pub async fn create_subgraph_client(
    http_client: reqwest::Client,
    graph_node: &GraphNodeConfig,
    subgraph_config: &SubgraphConfig,
) -> &'static SubgraphClient {
    Box::leak(Box::new(
        SubgraphClient::new(
            http_client,
            subgraph_config.deployment_id.map(|deployment| {
                DeploymentDetails::for_graph_node_url(
                    graph_node.status_url.clone(),
                    graph_node.query_url.clone(),
                    deployment,
                )
            }),
            DeploymentDetails::for_query_url_with_token(
                subgraph_config.query_url.clone(),
                subgraph_config.query_auth_token.clone(),
            ),
        )
        .await,
    ))
}
