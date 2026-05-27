// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! gRPC server for DIPS RCA proposals.
//!
//! This module implements the `IndexerDipsService` gRPC interface that receives
//! RecurringCollectionAgreement (RCA) proposals from the Dipper service.
//!
//! # Request Flow
//!
//! ```text
//! Dipper ──gRPC──> DipsServer::submit_agreement_proposal()
//!                        │
//!                        ├─ Version check (must be 2)
//!                        ├─ Size validation (non-empty, max 10KB)
//!                        ├─ Service-provider match
//!                        ├─ Timestamp validation (deadline, endsAt)
//!                        ├─ IPFS manifest fetch
//!                        ├─ Network validation
//!                        ├─ Price validation
//!                        │
//!                        └─> Store in pending_rca_proposals table
//!                                    │
//!                                    └─> Return Accept/Reject
//! ```
//!
//! Signature and signer-authorization checks are not performed here. The
//! on-chain `acceptIndexingAgreement` call verifies the signer (via either
//! an ECDSA signature or a pre-stored payer offer) when the indexer-agent
//! submits the acceptance transaction.
//!
//! # Response Behavior
//!
//! Returns `Accept` if the RCA passes all validation and is stored successfully.
//! Returns `Reject` if any validation fails. This enables the Dipper to reassign
//! the indexing request to another indexer on rejection.
//!
//! # Cancellation
//!
//! The `cancel_agreement` endpoint is unimplemented. Cancellation is handled
//! on-chain via the RecurringCollector contract, not through this gRPC interface.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use thegraph_core::alloy::primitives::Address;
use tonic::{Request, Response, Status};

use crate::{
    inflight::{InflightCounter, InflightGuard},
    ipfs::IpfsFetcher,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::{
        indexer_dips_service_server::IndexerDipsService, CancelAgreementRequest,
        CancelAgreementResponse, ProposalResponse, RejectReason, SubmitAgreementProposalRequest,
        SubmitAgreementProposalResponse,
    },
    store::RcaStore,
    DipsError,
};

/// Context for DIPS server with all validation dependencies.
///
/// Used for RCA validation:
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
    /// Network registry for supported networks
    pub registry: Arc<graph_networks_registry::NetworksRegistry>,
    /// Additional networks beyond the registry
    pub additional_networks: Arc<BTreeMap<String, String>>,
    /// Optional allowlist of payer addresses. When `Some`, proposals whose
    /// `payer` field is not in this set are rejected before any I/O. When
    /// `None`, every payer is accepted (legacy default).
    pub allowed_payers: Option<HashSet<Address>>,
}

/// DIPS server implementing RCA protocol.
///
/// Validates RecurringCollectionAgreement proposals before storage:
/// - Service-provider match
/// - IPFS manifest fetching and network validation
/// - Price minimum enforcement
///
/// Returns Accept/Reject to enable Dipper reassignment on rejection.
#[derive(Debug)]
pub struct DipsServer {
    pub ctx: Arc<DipsServerContext>,
    pub expected_payee: Address,
    /// Shared counter incremented for every request that enters the handler.
    /// The IPFS client reads it to decide whether to use the full retry
    /// budget or fall back to a single attempt under load.
    pub inflight: InflightCounter,
}

/// Map a DipsError to the appropriate RejectReason for the gRPC response.
fn reject_reason_from_error(err: &DipsError) -> RejectReason {
    match err {
        DipsError::TokensPerSecondTooLow { .. }
        | DipsError::TokensPerEntityPerSecondTooLow { .. } => RejectReason::PriceTooLow,
        DipsError::DeadlineExpired { .. } => RejectReason::DeadlineExpired,
        DipsError::AgreementExpired { .. } => RejectReason::AgreementExpired,
        DipsError::UnsupportedNetwork(_) => RejectReason::UnsupportedNetwork,
        DipsError::SubgraphManifestUnavailable(_) => RejectReason::SubgraphManifestUnavailable,
        DipsError::UnexpectedServiceProvider { .. } => RejectReason::UnexpectedServiceProvider,
        DipsError::UnsupportedMetadataVersion(_) => RejectReason::UnsupportedMetadataVersion,
        // Deliberately maps to Other so the wire response doesn't disclose
        // why the rejection happened — a caller probing for who is on the
        // allowlist would otherwise learn that by trial and error.
        DipsError::PayerNotAllowed(_) => RejectReason::Other,
        _ => RejectReason::Other,
    }
}

#[async_trait]
impl IndexerDipsService for DipsServer {
    /// Submit an RCA proposal.
    ///
    /// Validates:
    /// - Version 2 only
    /// - Service provider match
    /// - IPFS manifest and network compatibility
    /// - Price minimums
    ///
    /// On-chain offer existence is NOT checked — the offer doesn't exist yet
    /// at proposal time. The contract enforces it at `acceptIndexingAgreement`.
    ///
    /// Returns Accept/Reject based on validation results.
    async fn submit_agreement_proposal(
        &self,
        request: Request<SubmitAgreementProposalRequest>,
    ) -> Result<Response<SubmitAgreementProposalResponse>, Status> {
        let _guard = InflightGuard::new(self.inflight.clone());

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
        let deployment_id = crate::try_extract_deployment_id(&signed_voucher);
        match crate::validate_and_create_rca(self.ctx.clone(), &self.expected_payee, signed_voucher)
            .await
        {
            Ok(agreement_id) => {
                tracing::info!(%agreement_id, "RCA accepted");
                Ok(Response::new(SubmitAgreementProposalResponse {
                    response: ProposalResponse::Accept.into(),
                    reject_reason: RejectReason::Unspecified.into(),
                }))
            }
            Err(e) => {
                let reject_reason = reject_reason_from_error(&e);
                tracing::info!(
                    error = %e,
                    reason = ?reject_reason,
                    deployment_id = deployment_id.as_deref().unwrap_or("unknown"),
                    "RCA proposal rejected"
                );
                Ok(Response::new(SubmitAgreementProposalResponse {
                    response: ProposalResponse::Reject.into(),
                    reject_reason: reject_reason.into(),
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
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{ipfs::MockIpfsFetcher, price::PriceCalculator, store::InMemoryRcaStore};

    fn empty_counter() -> InflightCounter {
        Arc::new(AtomicUsize::new(0))
    }

    impl DipsServerContext {
        pub fn for_testing() -> Arc<Self> {
            use std::collections::{BTreeMap, HashSet};
            use thegraph_core::alloy::primitives::U256;

            Arc::new(Self {
                rca_store: Arc::new(InMemoryRcaStore::default()),
                ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
                price_calculator: Arc::new(PriceCalculator::new(
                    HashSet::from(["mainnet".to_string()]),
                    BTreeMap::from([("mainnet".to_string(), U256::from(200))]),
                    U256::from(100),
                )),
                registry: Arc::new(crate::registry::test_registry()),
                additional_networks: Arc::new(BTreeMap::new()),
                allowed_payers: None,
            })
        }
    }

    #[tokio::test]
    async fn test_empty_rejected() {
        // Arrange
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            inflight: empty_counter(),
        };
        let request = Request::new(SubmitAgreementProposalRequest {
            version: 2,
            signed_voucher: vec![],
        });

        // Act
        let err = server.submit_agreement_proposal(request).await.unwrap_err();

        // Assert
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_oversized_rejected() {
        // Arrange
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            inflight: empty_counter(),
        };
        let large_payload = vec![0u8; 10_001];
        let request = Request::new(SubmitAgreementProposalRequest {
            version: 2,
            signed_voucher: large_payload,
        });

        // Act
        let err = server.submit_agreement_proposal(request).await.unwrap_err();

        // Assert
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("exceeds maximum size"));
    }

    #[tokio::test]
    async fn test_unsupported_version_rejected() {
        // Arrange
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            inflight: empty_counter(),
        };
        let request = Request::new(SubmitAgreementProposalRequest {
            version: 1,
            signed_voucher: vec![1, 2, 3],
        });

        // Act
        let err = server.submit_agreement_proposal(request).await.unwrap_err();

        // Assert
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("Unsupported version"));
        assert!(err.message().contains("version 2"));
    }

    #[tokio::test]
    async fn test_cancel_unimplemented() {
        // Arrange
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            inflight: empty_counter(),
        };
        let request = Request::new(CancelAgreementRequest {
            version: 2,
            signed_cancellation: vec![],
        });

        // Act
        let err = server.cancel_agreement(request).await.unwrap_err();

        // Assert
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(err.message().contains("RecurringCollector"));
    }

    // =========================================================================
    // Tests for reject_reason_from_error
    // =========================================================================

    #[test]
    fn test_reject_reason_tokens_per_second_too_low() {
        // Arrange
        use thegraph_core::alloy::primitives::U256;
        let err = DipsError::TokensPerSecondTooLow {
            network: "mainnet".to_string(),
            minimum: U256::from(100),
            offered: U256::from(50),
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::PriceTooLow);
    }

    #[test]
    fn test_reject_reason_tokens_per_entity_per_second_too_low() {
        // Arrange
        use thegraph_core::alloy::primitives::U256;
        let err = DipsError::TokensPerEntityPerSecondTooLow {
            minimum: U256::from(100),
            offered: U256::from(10),
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::PriceTooLow);
    }

    #[test]
    fn test_reject_reason_unsupported_network() {
        // Arrange
        let err = DipsError::UnsupportedNetwork("unknown-network".to_string());

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::UnsupportedNetwork);
    }

    #[test]
    fn test_reject_reason_deadline_expired() {
        // Arrange
        let err = DipsError::DeadlineExpired {
            deadline: 1000,
            now: 2000,
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::DeadlineExpired);
    }

    #[test]
    fn test_reject_reason_abi_decoding() {
        // Arrange
        let err = DipsError::AbiDecoding("invalid bytes".to_string());

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::Other);
    }

    #[test]
    fn test_reject_reason_agreement_expired() {
        // Arrange
        let err = DipsError::AgreementExpired {
            ends_at: 1000,
            now: 2000,
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::AgreementExpired);
    }

    #[test]
    fn test_reject_reason_subgraph_manifest_unavailable() {
        // Arrange
        let err = DipsError::SubgraphManifestUnavailable("QmTest".to_string());

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::SubgraphManifestUnavailable);
    }

    #[test]
    fn test_reject_reason_unexpected_service_provider() {
        // Arrange
        let err = DipsError::UnexpectedServiceProvider {
            expected: Address::repeat_byte(0x01),
            actual: Address::repeat_byte(0x02),
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::UnexpectedServiceProvider);
    }

    #[test]
    fn test_reject_reason_unsupported_metadata_version() {
        // Arrange
        let err = DipsError::UnsupportedMetadataVersion(99);

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::UnsupportedMetadataVersion);
    }
}
