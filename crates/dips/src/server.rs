// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use thegraph_core::alloy::primitives::Address;
use tonic::{Request, Response, Status};

use crate::{
    ipfs::IpfsFetcher,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::{
        indexer_dips_service_server::IndexerDipsService, CancelAgreementRequest,
        CancelAgreementResponse, ProposalResponse, SubmitAgreementProposalRequest,
        SubmitAgreementProposalResponse,
    },
    signers::SignerValidator,
    store::RcaStore,
};

/// Context for DIPS server with all validation dependencies.
///
/// Used for RCA validation:
/// - Signature verification
/// - IPFS manifest fetching
/// - Price minimum enforcement
/// - Network registry lookups
#[derive(Debug, Clone)]
pub struct DipsServerContext {
    /// RCA store (seconds-based RCA)
    pub rca_store: Arc<dyn RcaStore>,
    /// IPFS client for fetching subgraph manifests
    pub ipfs_fetcher: Arc<dyn IpfsFetcher>,
    /// Price calculator for validating minimum prices
    pub price_calculator: Arc<PriceCalculator>,
    /// Signature validator for EIP-712 verification
    pub signer_validator: Arc<dyn SignerValidator>,
    /// Network registry for supported networks
    pub registry: Arc<graph_networks_registry::NetworksRegistry>,
    /// Additional networks beyond the registry
    pub additional_networks: Arc<BTreeMap<String, String>>,
}

/// DIPS server implementing RCA protocol.
///
/// Validates RecurringCollectionAgreement proposals before storage:
/// - EIP-712 signature verification
/// - IPFS manifest fetching and network validation
/// - Price minimum enforcement
///
/// Returns Accept/Reject to enable Dipper reassignment on rejection.
#[derive(Debug)]
pub struct DipsServer {
    pub ctx: Arc<DipsServerContext>,
    pub expected_payee: Address,
    pub chain_id: u64,
    /// RecurringCollector contract address for EIP-712 domain
    pub recurring_collector: Address,
}

#[async_trait]
impl IndexerDipsService for DipsServer {
    /// Submit an RCA proposal.
    ///
    /// Validates:
    /// - Version 2 only
    /// - EIP-712 signature
    /// - IPFS manifest and network compatibility
    /// - Price minimums
    ///
    /// Returns Accept/Reject based on validation results.
    async fn submit_agreement_proposal(
        &self,
        request: Request<SubmitAgreementProposalRequest>,
    ) -> Result<Response<SubmitAgreementProposalResponse>, Status> {
        let SubmitAgreementProposalRequest {
            version,
            signed_voucher,
        } = request.into_inner();

        // Only accept version 2
        if version != 2 {
            return Err(Status::invalid_argument(format!(
                "Unsupported version {}. Only version 2 (RecurringCollectionAgreement) is supported.",
                version
            )));
        }

        // Basic sanity checks
        if signed_voucher.is_empty() {
            return Err(Status::invalid_argument("signed_voucher cannot be empty"));
        }

        if signed_voucher.len() > 10_000 {
            return Err(Status::invalid_argument(
                "signed_voucher exceeds maximum size of 10KB",
            ));
        }

        // Validate and store RCA
        let domain = crate::rca_eip712_domain(self.chain_id, self.recurring_collector);
        match crate::validate_and_create_rca(
            self.ctx.clone(),
            &domain,
            &self.expected_payee,
            signed_voucher,
        )
        .await
        {
            Ok(agreement_id) => {
                tracing::info!(%agreement_id, "RCA accepted");
                Ok(Response::new(SubmitAgreementProposalResponse {
                    response: ProposalResponse::Accept.into(),
                }))
            }
            Err(e) => {
                tracing::warn!(error = %e, "RCA rejected");
                Ok(Response::new(SubmitAgreementProposalResponse {
                    response: ProposalResponse::Reject.into(),
                }))
            }
        }
    }

    /// Cancel agreement - unimplemented.
    ///
    /// Cancellation is handled on-chain via the RecurringCollector contract.
    async fn cancel_agreement(
        &self,
        _request: Request<CancelAgreementRequest>,
    ) -> Result<Response<CancelAgreementResponse>, Status> {
        Err(Status::unimplemented(
            "Cancellation is handled on-chain via RecurringCollector contract",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ipfs::MockIpfsFetcher, price::PriceCalculator, signers::NoopSignerValidator,
        store::InMemoryRcaStore,
    };

    impl DipsServerContext {
        pub fn for_testing() -> Arc<Self> {
            use std::collections::BTreeMap;
            use thegraph_core::alloy::primitives::U256;

            Arc::new(Self {
                rca_store: Arc::new(InMemoryRcaStore::default()),
                ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
                price_calculator: Arc::new(PriceCalculator::new(
                    BTreeMap::from([("mainnet".to_string(), U256::from(200))]),
                    U256::from(100),
                )),
                signer_validator: Arc::new(NoopSignerValidator),
                registry: Arc::new(crate::registry::test_registry()),
                additional_networks: Arc::new(BTreeMap::new()),
            })
        }
    }

    #[tokio::test]
    async fn test_empty_rejected() {
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            chain_id: 1,
            recurring_collector: Address::ZERO,
        };

        let request = Request::new(SubmitAgreementProposalRequest {
            version: 2,
            signed_voucher: vec![],
        });

        let err = server.submit_agreement_proposal(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_oversized_rejected() {
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            chain_id: 1,
            recurring_collector: Address::ZERO,
        };

        let large_payload = vec![0u8; 10_001];
        let request = Request::new(SubmitAgreementProposalRequest {
            version: 2,
            signed_voucher: large_payload,
        });

        let err = server.submit_agreement_proposal(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("exceeds maximum size"));
    }

    #[tokio::test]
    async fn test_unsupported_version_rejected() {
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            chain_id: 1,
            recurring_collector: Address::ZERO,
        };

        let request = Request::new(SubmitAgreementProposalRequest {
            version: 1,
            signed_voucher: vec![1, 2, 3],
        });

        let err = server.submit_agreement_proposal(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("Unsupported version"));
        assert!(err.message().contains("version 2"));
    }

    #[tokio::test]
    async fn test_cancel_unimplemented() {
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            chain_id: 1,
            recurring_collector: Address::ZERO,
        };

        let request = Request::new(CancelAgreementRequest {
            version: 2,
            signed_cancellation: vec![],
        });

        let err = server.cancel_agreement(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(err.message().contains("RecurringCollector"));
    }
}
