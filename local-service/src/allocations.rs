use serde::{Deserialize, Serialize};

use crate::keys;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Allocations {
    meta: Meta,
    allocations: Vec<AllocationFragment>,
}

impl Allocations {
    pub fn new(indexer: keys::Indexer) -> Self {
        Allocations {
            meta: Meta {
                block: Block {
                    number: 123,
                    hash: "0x0000000000000000000000000000000000000000000000000000000000000000"
                        .into(),
                    timestamp: "2021-01-01T00:00:00Z".into(),
                },
            },
            allocations: vec![AllocationFragment {
                id: "0x123".into(), // TODO: "<allocation id address>"
                indexer: Indexer {
                    id: indexer.address.to_string(),
                },
                allocated_tokens: "0".into(),
                created_at_block_hash:
                    "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
                created_at_epoch: "1".into(),
                closed_at_epoch: None,
                subgraph_deployment: SubgraphDeployment {
                    id: "QmUhiH6Z5xo6o3GNzsSvqpGKLmCt6w5WzKQ1yHk6C8AA8S".into(),
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
