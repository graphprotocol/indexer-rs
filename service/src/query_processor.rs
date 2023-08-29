// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use ethers_core::types::{Signature, U256};
use log::error;
use native::attestation::AttestationSigner;
use serde::{Deserialize, Serialize};
use tap_core::tap_manager::SignedReceipt;

use crate::attestation_signers::AttestationSigners;
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
    attestation_signers: AttestationSigners,
}

impl QueryProcessor {
    pub fn new(
        graph_node: GraphNodeInstance,
        attestation_signers: AttestationSigners,
    ) -> QueryProcessor {
        QueryProcessor {
            graph_node,
            attestation_signers,
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

        let signers = self.attestation_signers.read().await;
        let signer = signers.get(&allocation_id).ok_or_else(|| {
            QueryError::Other(anyhow::anyhow!(
                "No signer found for allocation id {}",
                allocation_id
            ))
        })?;

        let response = self
            .graph_node
            .subgraph_query_raw(&subgraph_deployment_id.ipfs_hash(), query.clone())
            .await?;

        let attestation_signature = response
            .attestable
            .then(|| Self::create_attestation(signer, query, &response));

        Ok(Response {
            result: QueryResult {
                graphql_response: response.graphql_response,
                attestation: attestation_signature,
            },
            status: 200,
        })
    }

    fn create_attestation(
        signer: &AttestationSigner,
        query: String,
        response: &UnattestedQueryResult,
    ) -> Signature {
        let attestation = signer.create_attestation(&query, &response.graphql_response);
        Signature {
            r: U256::from_big_endian(&attestation.r),
            s: U256::from_big_endian(&attestation.s),
            v: attestation.v as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethereum_types::Address;
    use hex_literal::hex;

    use crate::{
        common::allocation::{allocation_signer, Allocation, AllocationStatus, SubgraphDeployment},
        util::create_attestation_signer,
    };

    use super::*;

    const INDEXER_OPERATOR_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const INDEXER_ADDRESS: &str = "0x1234567890123456789012345678901234567890";

    #[test]
    fn hex_to_ipfs_multihash() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let expected_ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::from_hex(deployment_id)
                .unwrap()
                .ipfs_hash(),
            expected_ipfs_hash
        );
    }

    #[test]
    fn ipfs_multihash_to_hex() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::from_ipfs_hash(ipfs_hash)
                .unwrap()
                .to_string(),
            deployment_id
        );
    }

    #[test]
    fn subgraph_deployment_id_input_validation_success() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::new(ipfs_hash).unwrap().to_string(),
            deployment_id
        );

        assert_eq!(
            SubgraphDeploymentID::new(deployment_id)
                .unwrap()
                .ipfs_hash(),
            ipfs_hash
        );
    }

    #[test]
    fn subgraph_deployment_id_input_validation_fail() {
        let invalid_deployment_id =
            "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884a";
        let invalid_ipfs_hash = "Qm1234";

        let res = SubgraphDeploymentID::new(invalid_deployment_id);
        assert!(res.is_err());

        let res = SubgraphDeploymentID::new(invalid_ipfs_hash);
        assert!(res.is_err());
    }

    #[test]
    fn paid_query_attestation() {
        let subgraph_deployment_id = SubgraphDeploymentID::new(
            "0xc064c354bc21dd958b1d41b67b8ef161b75d2246b425f68ed4c74964ae705cbd",
        )
        .unwrap();

        let subgraph_deployment = SubgraphDeployment {
            id: subgraph_deployment_id.clone(),
            denied_at: None,
            staked_tokens: U256::from(0),
            signalled_tokens: U256::from(0),
            query_fees_amount: U256::from(0),
        };

        let allocation = &Allocation {
            id: Address::from_str("0x4CAF2827961262ADEF3D0Ad15C341e40c21389a4").unwrap(),
            status: AllocationStatus::Null,
            subgraph_deployment,
            indexer: Address::from_str(INDEXER_ADDRESS).unwrap(),
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

        let allocation_key = allocation_signer(INDEXER_OPERATOR_MNEMONIC, allocation).unwrap();

        let attestation_signer = create_attestation_signer(
            U256::from(1),
            Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap(),
            allocation_key,
            subgraph_deployment_id.bytes32(),
        )
        .unwrap();

        let attestation = QueryProcessor::create_attestation(
            &attestation_signer,
            "test input".to_string(),
            &UnattestedQueryResult {
                graphql_response: "test output".to_string(),
                attestable: true,
            },
        );

        // Values generated using https://github.com/graphprotocol/indexer/blob/f8786c979a8ed0fae93202e499f5ce25773af473/packages/indexer-native/lib/index.d.ts#L44
        let expected_signature = Signature {
            v: 27,
            r: hex!("a0c83c0785e2223ac1ea1eb9e4ffd4ca867275469a7b73dab24f39ddcdec5466").into(),
            s: hex!("4d0457efea889f2ec7ffcc7ff9b408428d0691356f34b01f419f7674d0eb4ddf").into(),
        };

        assert_eq!(attestation, expected_signature);
    }
}
