// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::U256;
use serde::{Deserialize, Deserializer};
use thegraph_core::{Address, DeploymentId};

pub mod monitor;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub id: Address,
    pub status: AllocationStatus,
    pub subgraph_deployment: SubgraphDeployment,
    pub indexer: Address,
    pub allocated_tokens: U256,
    pub created_at_epoch: u64,
    pub created_at_block_hash: String,
    pub closed_at_epoch: Option<u64>,
    pub closed_at_epoch_start_block_hash: Option<String>,
    pub previous_epoch_start_block_hash: Option<String>,
    pub poi: Option<String>,
    pub query_fee_rebates: Option<U256>,
    pub query_fees_collected: Option<U256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AllocationStatus {
    Null,
    Active,
    Closed,
    Finalized,
    Claimed,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct SubgraphDeployment {
    pub id: DeploymentId,
    #[serde(rename = "deniedAt")]
    pub denied_at: Option<u64>,
}

impl<'d> Deserialize<'d> for Allocation {
    fn deserialize<D>(deserializer: D) -> Result<Allocation, D::Error>
    where
        D: Deserializer<'d>,
    {
        #[derive(Deserialize)]
        struct InnerIndexer {
            id: Address,
        }

        #[derive(Deserialize)]
        #[allow(non_snake_case)]
        struct Outer {
            id: Address,
            subgraphDeployment: SubgraphDeployment,
            indexer: InnerIndexer,
            allocatedTokens: U256,
            createdAtBlockHash: String,
            createdAtEpoch: u64,
            closedAtEpoch: Option<u64>,
        }

        let outer = Outer::deserialize(deserializer)?;

        Ok(Allocation {
            id: outer.id,
            status: AllocationStatus::Null,
            subgraph_deployment: outer.subgraphDeployment,
            indexer: outer.indexer.id,
            allocated_tokens: outer.allocatedTokens,
            created_at_epoch: outer.createdAtEpoch,
            created_at_block_hash: outer.createdAtBlockHash,
            closed_at_epoch: outer.closedAtEpoch,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        })
    }
}
