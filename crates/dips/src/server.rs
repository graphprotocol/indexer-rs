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
//! Cancellation is handled entirely on-chain via the RecurringCollector contract;
//! there is no gRPC method for it. Dipper calls `cancelIndexingAgreementByPayer`
//! directly and indexer-agents observe `IndexingAgreementCanceled` events through
//! the indexing-payments subgraph.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use thegraph_core::alloy::{primitives::Address, sol_types::Eip712Domain};
use tonic::{Request, Response, Status};

use crate::{
    inflight::{InflightCounter, InflightGuard},
    ipfs::IpfsFetcher,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::{
        indexer_dips_service_server::IndexerDipsService,
        submit_agreement_proposal_response::Outcome, Accepted, RejectReason, Rejected,
        SubmitAgreementProposalRequest, SubmitAgreementProposalResponse,
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
    /// EIP-712 domain for recovering the RCA signer (RecurringCollector).
    pub rca_domain: Eip712Domain,
    /// Max DIPs agreements to store per rolling 24h window. None disables the cap.
    pub max_agreements_per_day: Option<u64>,
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

/// Classify a DipsError as the reason for the rejection. The match is exhaustive
/// so any future error variant must be classified here.
fn reject_reason_from_error(err: &DipsError) -> RejectReason {
    match err {
        DipsError::TokensPerSecondTooLow { .. }
        | DipsError::TokensPerEntityPerSecondTooLow { .. } => RejectReason::PriceTooLow,
        DipsError::DeadlineExpired { .. } => RejectReason::DeadlineExpired,
        DipsError::AgreementExpired { .. } => RejectReason::AgreementExpired,
        DipsError::UnsupportedNetwork(_) => RejectReason::UnsupportedNetwork,
        DipsError::SubgraphManifestUnavailable(_) => RejectReason::SubgraphManifestUnavailable,
        DipsError::InvalidSignature(_) => RejectReason::InvalidSignature,
        DipsError::UnexpectedServiceProvider { .. } => RejectReason::UnexpectedServiceProvider,
        DipsError::UnsupportedMetadataVersion(_) => RejectReason::UnsupportedMetadataVersion,
        DipsError::ManifestTooLarge { .. } => RejectReason::ManifestTooLarge,
        DipsError::CapacityExceeded { .. } => RejectReason::CapacityExceeded,
        // Malformed proposals with no dedicated reason map to the catch-all; the
        // detail carries the specifics.
        DipsError::AbiDecoding(_)
        | DipsError::InvalidSubgraphManifest(_)
        | DipsError::InvalidRca(_) => RejectReason::Unspecified,
        // A store failure means the proposal was valid but the indexer couldn't persist
        // it -- tell dipper this is transient so it retries rather than giving up.
        DipsError::UnknownError(_) => RejectReason::IndexerUnavailable,
    }
}

/// Human-readable detail sent to the caller. A store failure wraps a raw database
/// error that could leak schema internals, so it gets a generic message instead.
fn reject_detail_from_error(err: &DipsError) -> String {
    match err {
        DipsError::UnknownError(_) => "internal error while storing the proposal".to_string(),
        other => other.to_string(),
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
            signed_rca,
        } = request.into_inner();

        // Only accept version 2
        if version != 2 {
            return Err(Status::invalid_argument(format!(
                "Unsupported version {}. Only version 2 (RecurringCollectionAgreement) is supported.",
                version
            )));
        }

        // Basic sanity checks
        if signed_rca.is_empty() {
            return Err(Status::invalid_argument("signed_rca cannot be empty"));
        }

        if signed_rca.len() > 10_000 {
            return Err(Status::invalid_argument(
                "signed_rca exceeds maximum size of 10KB",
            ));
        }

        // Validate and store RCA
        let deployment_id = crate::try_extract_deployment_id(&signed_rca);
        match crate::validate_and_create_rca(self.ctx.clone(), &self.expected_payee, signed_rca)
            .await
        {
            Ok(agreement_id) => {
                tracing::info!(%agreement_id, "RCA accepted");
                Ok(Response::new(SubmitAgreementProposalResponse {
                    outcome: Some(Outcome::Accepted(Accepted {})),
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
                    outcome: Some(Outcome::Rejected(Rejected {
                        reason: reject_reason.into(),
                        detail: reject_detail_from_error(&e),
                    })),
                }))
            }
        }
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
                rca_domain: crate::rca_eip712_domain(1337, Address::repeat_byte(0xCC)),
                max_agreements_per_day: None,
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
            signed_rca: vec![],
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
            signed_rca: large_payload,
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
            signed_rca: vec![1, 2, 3],
        });

        // Act
        let err = server.submit_agreement_proposal(request).await.unwrap_err();

        // Assert
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("Unsupported version"));
        assert!(err.message().contains("version 2"));
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
    fn test_reject_reason_invalid_signature() {
        // Arrange
        let err = DipsError::InvalidSignature("bad signature".to_string());

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::InvalidSignature);
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

    #[test]
    fn test_reject_reason_malformed_proposals_map_to_unspecified() {
        // Arrange: malformed inputs share the UNSPECIFIED catch-all; detail carries the specifics.
        let abi = DipsError::AbiDecoding("invalid bytes".to_string());
        let manifest = DipsError::InvalidSubgraphManifest("QmTest".to_string());
        let rca = DipsError::InvalidRca("bad rca".to_string());

        // Act + Assert
        assert_eq!(
            super::reject_reason_from_error(&abi),
            RejectReason::Unspecified
        );
        assert_eq!(
            super::reject_reason_from_error(&manifest),
            RejectReason::Unspecified
        );
        assert_eq!(
            super::reject_reason_from_error(&rca),
            RejectReason::Unspecified
        );
    }

    #[test]
    fn test_reject_reason_manifest_too_large() {
        // Arrange
        let err = DipsError::ManifestTooLarge {
            file: "QmTest".to_string(),
            limit_bytes: 25 * 1024 * 1024,
        };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::ManifestTooLarge);
    }

    #[test]
    fn test_reject_reason_capacity_exceeded() {
        // Arrange
        let err = DipsError::CapacityExceeded { limit: 30 };

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert
        assert_eq!(reason, RejectReason::CapacityExceeded);
    }

    #[test]
    fn test_reject_reason_unknown_error_is_transient() {
        // Arrange: a store/database failure surfaces as UnknownError.
        let err = DipsError::UnknownError(anyhow::anyhow!("connection pool timed out"));

        // Act
        let reason = super::reject_reason_from_error(&err);

        // Assert: a valid proposal the indexer can't persist is transient, so the
        // sender should retry rather than treat it as a permanent failure.
        assert_eq!(reason, RejectReason::IndexerUnavailable);
    }

    #[test]
    fn test_reject_detail_sanitizes_store_errors() {
        // Arrange: the raw database error mentions schema internals.
        let store_err =
            DipsError::UnknownError(anyhow::anyhow!("relation \"users\" column secret"));
        let decode_err = DipsError::AbiDecoding("offset 7".to_string());

        // Act
        let store_detail = super::reject_detail_from_error(&store_err);
        let decode_detail = super::reject_detail_from_error(&decode_err);

        // Assert: the store error becomes generic while a well-formed validation
        // error keeps its descriptive detail.
        assert_eq!(store_detail, "internal error while storing the proposal");
        assert!(!store_detail.contains("users"));
        assert!(decode_detail.contains("offset 7"));
    }

    #[tokio::test]
    async fn test_reject_response_carries_reason_and_detail() {
        // Arrange: a non-decodable payload reaches validation and fails ABI decode.
        let ctx = DipsServerContext::for_testing();
        let server = DipsServer {
            ctx,
            expected_payee: Address::ZERO,
            inflight: empty_counter(),
        };
        let request = Request::new(SubmitAgreementProposalRequest {
            version: 2,
            signed_rca: vec![1, 2, 3],
        });

        // Act
        let response = server
            .submit_agreement_proposal(request)
            .await
            .unwrap()
            .into_inner();

        // Assert
        let Some(Outcome::Rejected(rejected)) = response.outcome else {
            panic!("expected a rejected outcome");
        };
        assert_eq!(rejected.reason, RejectReason::Unspecified as i32);
        assert!(rejected.detail.contains("ABI decoding"));
    }

    #[test]
    fn test_reject_reason_wire_names_round_trip() {
        let cases = [
            (RejectReason::Unspecified, "REJECT_REASON_UNSPECIFIED"),
            (RejectReason::PriceTooLow, "REJECT_REASON_PRICE_TOO_LOW"),
            (
                RejectReason::DeadlineExpired,
                "REJECT_REASON_DEADLINE_EXPIRED",
            ),
            (
                RejectReason::UnsupportedNetwork,
                "REJECT_REASON_UNSUPPORTED_NETWORK",
            ),
            (
                RejectReason::SubgraphManifestUnavailable,
                "REJECT_REASON_SUBGRAPH_MANIFEST_UNAVAILABLE",
            ),
            (
                RejectReason::UnexpectedServiceProvider,
                "REJECT_REASON_UNEXPECTED_SERVICE_PROVIDER",
            ),
            (
                RejectReason::AgreementExpired,
                "REJECT_REASON_AGREEMENT_EXPIRED",
            ),
            (
                RejectReason::UnsupportedMetadataVersion,
                "REJECT_REASON_UNSUPPORTED_METADATA_VERSION",
            ),
            (
                RejectReason::InvalidSignature,
                "REJECT_REASON_INVALID_SIGNATURE",
            ),
            (
                RejectReason::SenderNotTrusted,
                "REJECT_REASON_SENDER_NOT_TRUSTED",
            ),
            (
                RejectReason::CapacityExceeded,
                "REJECT_REASON_CAPACITY_EXCEEDED",
            ),
            (
                RejectReason::ManifestTooLarge,
                "REJECT_REASON_MANIFEST_TOO_LARGE",
            ),
            (
                RejectReason::ReplayDetected,
                "REJECT_REASON_REPLAY_DETECTED",
            ),
            (
                RejectReason::InsufficientEscrow,
                "REJECT_REASON_INSUFFICIENT_ESCROW",
            ),
            (
                RejectReason::IndexerUnavailable,
                "REJECT_REASON_INDEXER_UNAVAILABLE",
            ),
        ];
        for (variant, name) in cases {
            assert_eq!(variant.as_str_name(), name);
            assert_eq!(RejectReason::from_str_name(name), Some(variant));
        }
    }
}
