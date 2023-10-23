// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use alloy_primitives::Address;
use eventuals::Eventual;
use indexer_common::tap_manager::TapManager;
use log::error;
use serde::{Deserialize, Serialize};
use tap_core::tap_manager::SignedReceipt;
use toolshed::thegraph::attestation::Attestation;
use toolshed::thegraph::DeploymentId;

use crate::metrics;
use indexer_common::indexer_errors::IndexerErrorCode;
use indexer_common::prelude::AttestationSigner;

use crate::graph_node::GraphNodeInstance;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    #[serde(rename = "graphQLResponse")]
    pub graphql_response: String,
    pub attestation: Option<Attestation>,
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
    pub subgraph_deployment_id: DeploymentId,
    pub query: String,
}

/// Paid query needs subgraph_deployment_id, query, receipt
pub struct PaidQuery {
    pub subgraph_deployment_id: DeploymentId,
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

#[derive(Clone)]
pub struct QueryProcessor {
    graph_node: GraphNodeInstance,
    attestation_signers: Eventual<HashMap<Address, AttestationSigner>>,
    tap_manager: TapManager,
}

impl QueryProcessor {
    pub fn new(
        graph_node: GraphNodeInstance,
        attestation_signers: Eventual<HashMap<Address, AttestationSigner>>,
        tap_manager: TapManager,
    ) -> QueryProcessor {
        QueryProcessor {
            graph_node,
            attestation_signers,
            tap_manager,
        }
    }

    pub async fn execute_free_query(
        &self,
        query: FreeQuery,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self
            .graph_node
            .subgraph_query_raw(&query.subgraph_deployment_id, query.query)
            .await
            .map_err(|e| {
                metrics::INDEXER_ERROR
                    .with_label_values(&[&IndexerErrorCode::IE033.to_string()])
                    .inc();

                e
            })?;

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

        let parsed_receipt: SignedReceipt = match serde_json::from_str(&receipt)
            .map_err(|e| QueryError::Other(anyhow::Error::from(e)))
        {
            Ok(r) => r,
            Err(e) => {
                metrics::INDEXER_ERROR
                    .with_label_values(&[&IndexerErrorCode::IE031.to_string()])
                    .inc();

                return Err(e);
            }
        };

        let allocation_id = parsed_receipt.message.allocation_id;

        self.tap_manager
            .verify_and_store_receipt(parsed_receipt)
            .await
            .map_err(|e| {
                //TODO: fit indexer errors to TAP better, currently keeping the old messages
                metrics::INDEXER_ERROR
                    .with_label_values(&[&IndexerErrorCode::IE053.to_string()])
                    .inc();

                QueryError::Other(e)
            })?;

        let signers = self
            .attestation_signers
            .value_immediate()
            .ok_or_else(|| QueryError::Other(anyhow::anyhow!("System is not ready yet")))?;
        let signer = signers.get(&allocation_id).ok_or_else(|| {
            metrics::INDEXER_ERROR
                .with_label_values(&[&IndexerErrorCode::IE022.to_string()])
                .inc();

            QueryError::Other(anyhow::anyhow!(
                "No signer found for allocation id {}",
                allocation_id
            ))
        })?;

        let response = self
            .graph_node
            .subgraph_query_raw(&subgraph_deployment_id, query.clone())
            .await?;

        let attestation = response
            .attestable
            .then(|| Self::create_attestation(signer, &query, &response));

        Ok(Response {
            result: QueryResult {
                graphql_response: response.graphql_response,
                attestation,
            },
            status: 200,
        })
    }

    fn create_attestation(
        signer: &AttestationSigner,
        query: &str,
        response: &UnattestedQueryResult,
    ) -> Attestation {
        signer.create_attestation(query, &response.graphql_response)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use alloy_primitives::Address;
    use ethers_core::types::U256;
    use indexer_common::prelude::{
        Allocation, AllocationStatus, AttestationSigner, SubgraphDeployment,
    };
    use lazy_static::lazy_static;

    use super::*;

    const INDEXER_OPERATOR_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const INDEXER_ADDRESS: &str = "0x1234567890123456789012345678901234567890";

    lazy_static! {
        static ref DEPLOYMENT_ID: DeploymentId = DeploymentId(
            "0xc064c354bc21dd958b1d41b67b8ef161b75d2246b425f68ed4c74964ae705cbd"
                .parse()
                .unwrap(),
        );
    }

    #[test]
    fn paid_query_attestation() {
        let subgraph_deployment = SubgraphDeployment {
            id: *DEPLOYMENT_ID,
            denied_at: None,
        };

        let indexer = Address::from_str(INDEXER_ADDRESS).unwrap();
        let allocation = &Allocation {
            id: Address::from_str("0x4CAF2827961262ADEF3D0Ad15C341e40c21389a4").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment,
            indexer,
            allocated_tokens: U256::from(100),
            created_at_epoch: 940,
            created_at_block_hash: String::from(""),
            closed_at_epoch: None,
            closed_at_epoch_start_block_hash: None,
            previous_epoch_start_block_hash: None,
            poi: None,
            query_fee_rebates: None,
            query_fees_collected: None,
        };

        let attestation_signer = AttestationSigner::new(
            INDEXER_OPERATOR_MNEMONIC,
            allocation,
            U256::from(1),
            Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
        )
        .unwrap();

        let request = "test input";
        let response = "test output";
        let attestation = QueryProcessor::create_attestation(
            &attestation_signer,
            request,
            &UnattestedQueryResult {
                graphql_response: response.to_owned(),
                attestable: true,
            },
        );

        attestation_signer
            .verify(&attestation, request, response, &allocation.id)
            .unwrap();
    }
}
