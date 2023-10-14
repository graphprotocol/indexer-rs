// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::{Address, U256};
use alloy_sol_types::Eip712Domain;
use ethers_core::k256::ecdsa::SigningKey;
use toolshed::thegraph::attestation::{self, Attestation};
use toolshed::thegraph::DeploymentId;

/// An attestation signer tied to a specific allocation via its signer key
#[derive(Debug, Clone)]
pub struct AttestationSigner {
    subgraph_deployment_id: DeploymentId,
    domain: Eip712Domain,
    signer: SigningKey,
}

impl AttestationSigner {
    pub fn new(
        chain_id: ethers_core::types::U256,
        dispute_manager: Address,
        signer: SigningKey,
        deployment_id: DeploymentId,
    ) -> Self {
        let mut chain_id_buf = [0_u8; 32];
        chain_id.to_big_endian(&mut chain_id_buf);
        let chain_id = U256::from_be_bytes(chain_id_buf);
        Self {
            subgraph_deployment_id: deployment_id,
            domain: attestation::eip712_domain(chain_id, dispute_manager),
            signer,
        }
    }

    pub fn create_attestation(&self, request: &str, response: &str) -> Attestation {
        attestation::create(
            &self.domain,
            &self.signer,
            &self.subgraph_deployment_id,
            request,
            response,
        )
    }

    pub fn verify(
        &self,
        attestation: &Attestation,
        request: &str,
        response: &str,
        expected_signer: &Address,
    ) -> Result<(), attestation::VerificationError> {
        attestation::verify(
            &self.domain,
            attestation,
            expected_signer,
            request,
            response,
        )
    }
}
