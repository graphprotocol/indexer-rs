// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{http::StatusCode, response::IntoResponse, Extension, Json};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    common::indexer_error::{IndexerError, IndexerErrorCause},
    server::{routes::internal_server_error_response, ServerOptions},
};

/// Parse an incoming query request and route queries with authenticated
/// free query token to graph node
/// Later add receipt manager functions for paid queries
pub async fn deployment_health(
    Extension(server): Extension<ServerOptions>,
    deployment: axum::extract::Path<String>,
) -> impl IntoResponse {
    // Create the GraphQL query
    let query = status_query(deployment.to_string());

    // Send the GraphQL request
    let response = reqwest::Client::new()
        .post(server.graph_node_status_endpoint)
        .header("Content-Type", "application/json")
        .json(&query)
        .send()
        .await;

    match response {
        Ok(response) => {
            if response.status().is_success() {
                // Deserialize the JSON response
                //TODO: match with error
                let data: serde_json::Value = if let Ok(data) = response.json().await {
                    data
                } else {
                    return internal_server_error_response("Invalid json response");
                };

                // Process the response and return the appropriate HTTP status
                let status = if let Some(status) =
                    data["data"]["indexingStatuses"].get(0).and_then(|s| {
                        let parse = serde_json::from_value::<IndexingStatus>(s.clone());
                        parse.ok()
                    }) {
                    status
                } else {
                    return internal_server_error_response("Missing indexing status");
                };

                // Build health response based on the returned status
                if status.health == SubgraphHealth::failed {
                    return internal_server_error_response("Subgraph deployment has failed");
                }

                if let Ok((latest, head)) = block_numbers(status) {
                    if latest > head - 5 {
                        (StatusCode::OK, Json("Subgraph deployment is up to date")).into_response()
                    } else {
                        internal_server_error_response("Subgraph deployment is lagging behind")
                    }
                } else {
                    internal_server_error_response(
                        "Invalid indexing status (missing block numbers)",
                    )
                }
            } else {
                internal_server_error_response("Unknown error")
            }
        }
        Err(e) => internal_server_error_response(&e.to_string()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexingStatus {
    health: SubgraphHealth,
    chains: Vec<ChainStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(non_camel_case_types)] // Need exact field names to match with GQL response
enum SubgraphHealth {
    healthy,
    unhealthy,
    failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainStatus {
    network: String,
    latest_block: Block,
    chain_head_block: Block,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Block {
    number: String,
    hash: String,
}

fn status_query(deployment: String) -> serde_json::Value {
    json!({
        "query": r#"query indexingStatus($subgraphs: [String!]!) {
            indexingStatuses(subgraphs: $subgraphs) {
                subgraph
                health
                chains {
                    network
                    ... on EthereumIndexingStatus {
                        latestBlock { number hash }
                        chainHeadBlock { number hash }
                    }
                }
            }
        }"#,
        "variables": {
            "subgraphs": [deployment],
        },
    })
}

fn block_numbers(status: IndexingStatus) -> Result<(u64, u64), IndexerError> {
    let latest_block_number = status
        .chains
        .get(0)
        .map(|chain| chain.latest_block.number.clone())
        .map(|number| number.parse::<u64>());

    let head_block_number = status
        .chains
        .get(0)
        .map(|chain| chain.chain_head_block.number.clone())
        .map(|number| number.parse::<u64>());

    if let (Some(Ok(latest)), Some(Ok(head))) = (latest_block_number, head_block_number) {
        Ok((latest, head))
    } else {
        Err(IndexerError::new(
            crate::common::indexer_error::IndexerErrorCode::IE018,
            Some(IndexerErrorCause::new(
                "Ill formatted block numbers from indexing status",
            )),
        ))
    }
}
