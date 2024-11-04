// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_common::prelude::SubgraphDeployment;
use serde::{Deserialize, Serialize};

use crate::{bootstrap::CREATED_AT_BLOCK_HASH, keys::Signer};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Allocations {
    meta: Meta,
    allocations: Vec<AllocationFragment>,
}

impl Allocations {
    pub fn new(indexer: Signer) -> Self {
        Allocations {
            meta: Meta {
                block: Block {
                    number: 123,
                    hash: CREATED_AT_BLOCK_HASH.into(),
                    timestamp: "2021-01-01T00:00:00Z".into(),
                },
            },
            allocations: vec![AllocationFragment {
                id: indexer.allocation.id.to_string(),
                indexer: Indexer {
                    id: indexer.allocation.indexer.to_string(),
                },
                allocated_tokens: indexer.allocation.allocated_tokens.to_string(),
                created_at_block_hash: indexer.allocation.created_at_block_hash,
                created_at_epoch: indexer.allocation.created_at_epoch.to_string(),
                closed_at_epoch: indexer
                    .allocation
                    .closed_at_epoch
                    .map(|epoch| epoch.to_string()),
                subgraph_deployment: indexer.allocation.subgraph_deployment,
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
struct Meta {
    block: Block,
}

#[derive(Debug, Serialize, Deserialize)]
struct Block {
    number: u128,
    hash: String,
    timestamp: String,
}
