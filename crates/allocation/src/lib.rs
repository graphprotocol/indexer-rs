// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use indexer_query::allocations_query;
use serde::{Deserialize, Deserializer};
use thegraph_core::{
    alloy::primitives::{Address, U256},
    DeploymentId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub id: Address,
    // True when this allocation belongs to Legacy (V1) TAP, false for Horizon (V2)
    pub is_legacy: bool,
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
            #[allow(non_snake_case)]
            isLegacy: bool,
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
            is_legacy: outer.isLegacy,
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

impl TryFrom<allocations_query::AllocationFragment> for Allocation {
    type Error = anyhow::Error;

    fn try_from(
        value: allocations_query::AllocationsQueryAllocations,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Address::from_str(&value.id)?,
            // graphql_client converts `isLegacy` to `is_legacy`
            is_legacy: value.is_legacy,
            status: AllocationStatus::Null,
            subgraph_deployment: SubgraphDeployment {
                id: DeploymentId::from_str(&value.subgraph_deployment.id)?,
                denied_at: Some(value.subgraph_deployment.denied_at as u64),
            },
            indexer: Address::from_str(&value.indexer.id)?,
            allocated_tokens: value.allocated_tokens,
            created_at_epoch: value.created_at_epoch as u64,
            created_at_block_hash: value.created_at_block_hash.to_string(),
            closed_at_epoch: value.closed_at_epoch.map(|v| v as u64),
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        })
    }
}
