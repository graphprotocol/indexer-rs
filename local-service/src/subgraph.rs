use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

pub(crate) const NETWORK_SUBGRAPH_ROUTE: &str = "/network_subgraph";

pub(crate) async fn network_subgraph() -> impl IntoResponse {
    Json(GraphqlResponse::Allocations(Allocations {
        ..Default::default()
    }))
}

#[derive(Debug, Serialize, Deserialize)]
enum GraphqlResponse {
    Allocations(Allocations),
}

#[derive(Debug, Serialize, Deserialize)]
struct Allocations {
    meta: Meta,
    allocations: Vec<AllocationFragment>,
}

impl Default for Allocations {
    fn default() -> Self {
        Allocations {
            meta: Meta {
                block: Block {
                    number: 1,
                    hash: "0x123".into(),
                    timestamp: "2021-01-01T00:00:00Z".into(),
                },
            },
            allocations: vec![AllocationFragment {
                id: "0x123".into(),
                indexer: Indexer { id: "0x456".into() },
                allocated_tokens: "100".into(),
                created_at_block_hash: "0x789".into(),
                created_at_epoch: "1".into(),
                closed_at_epoch: None,
                subgraph_deployment: SubgraphDeployment {
                    id: "0xabc".into(),
                    denied_at: "0".into(),
                },
            }],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AllocationFragment {
    id: String,
    indexer: Indexer,
    allocated_tokens: String,
    created_at_block_hash: String,
    created_at_epoch: String,
    closed_at_epoch: Option<String>,
    subgraph_deployment: SubgraphDeployment,
}

#[derive(Debug, Serialize, Deserialize)]
struct Indexer {
    id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SubgraphDeployment {
    id: String,
    denied_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    block: Block,
}

#[derive(Debug, Serialize, Deserialize)]
struct Block {
    number: u128,
    hash: String,
    timestamp: String,
}
