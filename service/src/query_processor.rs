// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use ethers_core::types::Address;
use ethers_core::types::{Signature, U256};
use log::error;
use native::attestation::AttestationSigner;
use serde::{Deserialize, Serialize};
use tap_core::tap_manager::SignedReceipt;

use crate::common::types::SubgraphDeploymentID;
use crate::graph_node::GraphNodeInstance;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    #[serde(rename = "graphQLResponse")]
    pub graphql_response: String,
    pub attestation: Option<Signature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnattestedQueryResult {
    #[serde(rename = "graphQLResponse")]
    pub graphql_response: String,
    pub attestable: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response<T> {
    pub result: T,
    pub status: i64,
}

/// Free query do not need signature, receipt, signers
/// Also ignore metrics for now
/// Later add along with PaidQuery
#[derive(Debug)]
pub struct FreeQuery {
    pub subgraph_deployment_id: SubgraphDeploymentID,
    pub query: String,
}

/// Paid query needs subgraph_deployment_id, query, receipt
pub struct PaidQuery {
    pub subgraph_deployment_id: SubgraphDeploymentID,
    pub query: String,
    pub receipt: String,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error("The subgraph is in a failed state")]
    IndexingError,
    #[error("Bad or invalid entity data found in the subgraph: {}", .0.to_string())]
    BadData(anyhow::Error),
    #[error("Unknown error: {0}")]
    Other(anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct QueryProcessor {
    graph_node: GraphNodeInstance,
    signers: HashMap<Address, AttestationSigner>,
}

impl QueryProcessor {
    pub fn new(graph_node: GraphNodeInstance) -> QueryProcessor {
        QueryProcessor {
            graph_node,
            // TODO: populate signers
            signers: HashMap::new(),
        }
    }

    pub async fn execute_free_query(
        &self,
        query: FreeQuery,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self
            .graph_node
            .subgraph_query_raw(&query.subgraph_deployment_id.ipfs_hash(), query.query)
            .await?;

        Ok(Response {
            result: response,
            status: 200,
        })
    }

    pub async fn execute_paid_query(
        &self,
        query: PaidQuery,
    ) -> Result<Response<QueryResult>, QueryError> {
        let PaidQuery {
            subgraph_deployment_id,
            query,
            receipt,
        } = query;

        // TODO: Emit IndexerErrorCode::IE031 on error
        let parsed_receipt: SignedReceipt = serde_json::from_str(&receipt)
            .map_err(|e| QueryError::Other(anyhow::Error::from(e)))?;

        let allocation_id = parsed_receipt.message.allocation_id;

        // TODO: Handle the TAP receipt

        let signer = self.signers.get(&allocation_id).ok_or_else(|| {
            QueryError::Other(anyhow::anyhow!(
                "No signer found for allocation id {}",
                allocation_id
            ))
        })?;

        let response = self
            .graph_node
            .subgraph_query_raw(&subgraph_deployment_id.ipfs_hash(), query.clone())
            .await?;

        let attestation_signature = response.attestable.then(|| {
            // TODO: Need to check correctness of this. In particular, the endianness.
            let attestation = signer.create_attestation(&query, &response.graphql_response);
            Signature {
                r: U256::from_big_endian(&attestation.r),
                s: U256::from_big_endian(&attestation.s),
                v: attestation.v as u64,
            }
        });

        Ok(Response {
            result: QueryResult {
                graphql_response: response.graphql_response,
                attestation: attestation_signature,
            },
            status: 200,
        })
    }
}
