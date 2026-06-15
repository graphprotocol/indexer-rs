// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! DIPS (Direct Indexer Payments) for The Graph.
//!
//! This crate implements the indexer-side handling of RecurringCollectionAgreement (RCA)
//! proposals. When a payer wants indexing services, the Dipper service creates and signs
//! an RCA on their behalf, then sends it to the indexer via gRPC.
//!
//! # Architecture
//!
//! ```text
//! Payer (user) ──deposits──> PaymentsEscrow contract
//!       │                           │
//!       │ authorizes signer         │ escrow data indexed
//!       ▼                           ▼
//!    Dipper ───SignedRCA───> indexer-rs (this crate)
//!       │                           │
//!       │                           │ validates & stores
//!       │                           ▼
//!       │                    pending_rca_proposals table
//!       │                           │
//!       │                           │ agent queries & decides
//!       │                           ▼
//!       └──────────────────> on-chain acceptance
//! ```
//!
//! # Validation Flow
//!
//! When an RCA arrives, this crate validates:
//! 1. **Service provider** - RCA is addressed to this indexer
//! 2. **Timestamps** - Deadline and end time haven't passed
//! 3. **Replay/idempotency** - Once the deterministic agreement id is derived,
//!    a re-sent proposal is resolved against the store before any IPFS fetch
//! 4. **IPFS manifest** - Subgraph deployment exists and is parseable
//! 5. **Network** - Subgraph's network is supported by this indexer
//! 6. **Pricing** - Offered price meets indexer's minimum
//!
//! Signature and signer-authorization checks are NOT performed here. With the
//! switch to offer-based authorization, the on-chain `acceptIndexingAgreement`
//! call verifies the signer (via either an ECDSA signature or a pre-stored
//! payer offer) when the indexer-agent submits the acceptance transaction.
//!
//! # Modules
//!
//! - [`server`] - gRPC server handling RCA proposals
//! - [`store`] - Storage trait for RCA proposals
//! - [`database`] - PostgreSQL implementation
//! - [`signers`] - Signer authorization via escrow accounts
//! - [`ipfs`] - IPFS client for subgraph manifests
//! - [`price`] - Minimum price enforcement

use std::sync::Arc;

use server::DipsServerContext;
use thegraph_core::alloy::{
    core::primitives::Address,
    primitives::{keccak256, ruint::aliases::U256, Signature, Uint},
    signers::SignerSync,
    sol,
    sol_types::{eip712_domain, Eip712Domain, SolStruct, SolValue},
};

#[cfg(feature = "db")]
pub mod database;
pub mod eip5267;
pub mod inflight;
pub mod ipfs;
#[cfg(any(feature = "rpc", feature = "db"))]
pub mod metrics;
pub mod price;
#[cfg(feature = "rpc")]
pub mod proto;
#[cfg(test)]
mod registry;
#[cfg(feature = "rpc")]
pub mod server;
pub mod store;
pub mod trusted_signers;

use thiserror::Error;
use uuid::Uuid;

pub use crate::eip5267::fetch_rca_eip712_domain;

/// Protocol version (seconds-based RCA)
pub const PROTOCOL_VERSION: u64 = 2;

sol! {
    // === RCA Types (seconds-based RecurringCollectionAgreement) ===

    /// The on-chain RecurringCollectionAgreement type.
    ///
    /// Matches `IRecurringCollector.RecurringCollectionAgreement` exactly.
    /// The agreement ID is derived on-chain via
    /// `bytes16(keccak256(abi.encode(payer, dataService, serviceProvider, deadline, nonce)))`.
    /// Note: `conditions` is NOT included in the agreement ID preimage.
    #[derive(Debug, PartialEq)]
    struct RecurringCollectionAgreement {
        uint64 deadline;
        uint64 endsAt;
        address payer;
        address dataService;
        address serviceProvider;
        uint256 maxInitialTokens;
        uint256 maxOngoingTokensPerSecond;
        uint32 minSecondsPerCollection;
        uint32 maxSecondsPerCollection;
        uint16 conditions;
        uint256 nonce;
        bytes metadata;
    }

    /// Wrapper pairing an RCA with its EIP-712 signature.
    #[derive(Debug, PartialEq)]
    struct SignedRecurringCollectionAgreement {
        RecurringCollectionAgreement agreement;
        bytes signature;
    }

    /// Metadata for indexing agreement acceptance, ABI-encoded into
    /// `RecurringCollectionAgreement.metadata`.
    #[derive(Debug, PartialEq)]
    struct AcceptIndexingAgreementMetadata {
        bytes32 subgraphDeploymentId;
        uint8 version;
        bytes terms;
    }

    /// Pricing terms, ABI-encoded into `AcceptIndexingAgreementMetadata.terms`.
    #[derive(Debug, PartialEq)]
    struct IndexingAgreementTermsV1 {
        uint256 tokensPerSecond;
        uint256 tokensPerEntityPerSecond;
    }

}

/// Derive the agreement ID deterministically from the RCA fields.
///
/// Matches the on-chain derivation:
///   `bytes16(keccak256(abi.encode(payer, dataService, serviceProvider, deadline, nonce)))`
fn derive_agreement_id(rca: &RecurringCollectionAgreement) -> Uuid {
    let encoded = (
        rca.payer,
        rca.dataService,
        rca.serviceProvider,
        rca.deadline,
        rca.nonce,
    )
        .abi_encode();
    let hash = keccak256(&encoded);
    let mut id_bytes = [0u8; 16];
    id_bytes.copy_from_slice(&hash[..16]);
    Uuid::from_bytes(id_bytes)
}

/// EIP-712 domain for RecurringCollectionAgreement signatures. Must match the
/// domain dipper signs with and the on-chain RecurringCollector contract; any
/// drift in name, version, chain id, or address makes every recovered signer wrong.
pub fn rca_eip712_domain(chain_id: u64, recurring_collector: Address) -> Eip712Domain {
    eip712_domain! {
        name: "RecurringCollector",
        version: "1",
        chain_id: chain_id,
        verifying_contract: recurring_collector,
    }
}

#[derive(Error, Debug)]
pub enum DipsError {
    // RCA validation
    #[error("invalid signature: {0}")]
    InvalidSignature(String),
    #[error("RCA service provider {actual} does not match the expected address {expected}")]
    UnexpectedServiceProvider { expected: Address, actual: Address },
    #[error("cannot get subgraph manifest for {0}")]
    SubgraphManifestUnavailable(String),
    #[error("invalid subgraph id {0}")]
    InvalidSubgraphManifest(String),
    #[error("subgraph manifest for {file} exceeds the {limit_bytes} byte cap")]
    ManifestTooLarge { file: String, limit_bytes: usize },
    #[error("network {0} is not supported")]
    UnsupportedNetwork(String),
    #[error(
        "tokens per second {offered} is below configured minimum {minimum} for network {network}"
    )]
    TokensPerSecondTooLow {
        network: String,
        minimum: U256,
        offered: U256,
    },
    #[error("tokens per entity per second {offered} is below configured minimum {minimum}")]
    TokensPerEntityPerSecondTooLow { minimum: U256, offered: U256 },
    // misc
    #[error("unknown error: {0}")]
    UnknownError(#[from] anyhow::Error),
    #[error("ABI decoding error: {0}")]
    AbiDecoding(String),
    #[error("invalid RCA: {0}")]
    InvalidRca(String),
    #[error("unsupported metadata version: {0}")]
    UnsupportedMetadataVersion(u8),
    #[error("agreement deadline {deadline} has already passed (current time: {now})")]
    DeadlineExpired { deadline: u64, now: u64 },
    #[error("agreement end time {ends_at} has already passed (current time: {now})")]
    AgreementExpired { ends_at: u64, now: u64 },
    #[error("sender {signer} is not authorised to send agreement proposals")]
    SenderNotTrusted { signer: Address },
    #[error("could not verify sender authorisation: {0}")]
    TrustVerificationUnavailable(String),
    #[error("indexer is at its DIPs agreement capacity ({limit} per 24h)")]
    CapacityExceeded { limit: u64 },
    // Replay detection (early dedup, before the IPFS fetch).
    #[error("agreement {agreement_id} was already rejected")]
    ReplayRejected { agreement_id: Uuid },
    #[error("agreement id {agreement_id} already stored with a different payload")]
    ReplayConflict { agreement_id: Uuid },
}

#[cfg(feature = "rpc")]
impl From<DipsError> for tonic::Status {
    fn from(value: DipsError) -> Self {
        tonic::Status::internal(format!("{value}"))
    }
}

// === RCA Implementations ===

impl RecurringCollectionAgreement {
    pub fn sign<S: SignerSync>(
        &self,
        domain: &Eip712Domain,
        signer: S,
    ) -> anyhow::Result<SignedRecurringCollectionAgreement> {
        let signed_rca = SignedRecurringCollectionAgreement {
            agreement: self.clone(),
            signature: signer.sign_typed_data_sync(self, domain)?.as_bytes().into(),
        };

        Ok(signed_rca)
    }
}

impl SignedRecurringCollectionAgreement {
    /// Recover the EIP-712 signer of the RCA over the RecurringCollector domain.
    /// An empty, malformed, or non-recovering signature is rejected as
    /// InvalidSignature. The recovered address is what PR B gates on.
    pub fn recover_signer(&self, domain: &Eip712Domain) -> Result<Address, DipsError> {
        if self.signature.is_empty() {
            return Err(DipsError::InvalidSignature("missing signature".to_string()));
        }
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;
        signature
            .recover_address_from_prehash(&self.agreement.eip712_signing_hash(domain))
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))
    }

    /// Validate proposal-time fields.
    ///
    /// Checks that the service provider matches the expected indexer
    /// address. On-chain offer existence is NOT checked here — the offer
    /// does not exist yet at proposal time. The contract enforces offer
    /// existence when the indexer-agent calls `acceptIndexingAgreement`.
    pub fn validate(&self, expected_service_provider: &Address) -> Result<(), DipsError> {
        if !self.agreement.serviceProvider.eq(expected_service_provider) {
            return Err(DipsError::UnexpectedServiceProvider {
                expected: *expected_service_provider,
                actual: self.agreement.serviceProvider,
            });
        }
        Ok(())
    }

    pub fn encode_vec(&self) -> Vec<u8> {
        self.abi_encode()
    }
}

/// Convert bytes32 subgraph deployment ID to IPFS CIDv0 string.
///
/// IPFS CIDv0 format: Qm... (base58-encoded multihash)
/// Multihash format: 0x12 (sha256) + 0x20 (32 bytes) + hash
fn bytes32_to_ipfs_hash(bytes: &[u8; 32]) -> String {
    // Prepend multihash prefix: 0x12 (sha256) + 0x20 (32 bytes length)
    let mut multihash = vec![0x12, 0x20];
    multihash.extend_from_slice(bytes);

    // Base58 encode
    bs58::encode(&multihash).into_string()
}

/// Try to extract the deployment ID from raw signed RCA bytes.
///
/// Best-effort: returns `None` if any decoding step fails.
pub(crate) fn try_extract_deployment_id(rca_bytes: &[u8]) -> Option<String> {
    let signed_rca = SignedRecurringCollectionAgreement::abi_decode(rca_bytes).ok()?;
    let metadata =
        AcceptIndexingAgreementMetadata::abi_decode(signed_rca.agreement.metadata.as_ref()).ok()?;
    Some(bytes32_to_ipfs_hash(&metadata.subgraphDeploymentId.0))
}

/// Window over which the cap counts live agreements (pending or accepted).
const CAPACITY_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Validate and create a RecurringCollectionAgreement.
///
/// Performs validation:
/// - Service provider match
/// - Deadline and expiry checks
/// - Replay/idempotency check on the derived agreement id (before any IPFS fetch)
/// - IPFS manifest fetching and network validation
/// - Price minimum enforcement
///
/// On-chain offer existence is NOT checked here — the offer doesn't exist
/// yet at proposal time. The contract enforces it at `acceptIndexingAgreement`.
///
/// Returns the agreement ID if successful, stores in database.
pub async fn validate_and_create_rca(
    ctx: Arc<DipsServerContext>,
    expected_service_provider: &Address,
    rca_bytes: Vec<u8>,
) -> Result<Uuid, DipsError> {
    let DipsServerContext {
        rca_store,
        ipfs_fetcher,
        price_calculator,
        registry,
        additional_networks,
        rca_domain,
        trusted_signers,
        max_new_agreements_per_24h,
    } = ctx.as_ref();

    // Decode SignedRCA
    let signed_rca = SignedRecurringCollectionAgreement::abi_decode(rca_bytes.as_ref())
        .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

    // Authenticate then authorize the sender: recover the EIP-712 signer, then
    // require it to hold the on-chain agreement-manager role before doing any work.
    let signer = signed_rca.recover_signer(rca_domain)?;
    tracing::debug!(%signer, "recovered RCA signer");
    trusted_signers.verify_trusted(signer).await?;

    // Validate service provider
    signed_rca.validate(expected_service_provider)?;

    // Validate deadline hasn't passed
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();

    let deadline: u64 = signed_rca.agreement.deadline;
    if deadline < now {
        return Err(DipsError::DeadlineExpired { deadline, now });
    }

    // Validate agreement hasn't already expired
    let ends_at: u64 = signed_rca.agreement.endsAt;
    if ends_at < now {
        return Err(DipsError::AgreementExpired { ends_at, now });
    }

    // Derive agreement ID deterministically from the RCA fields
    let agreement_id = derive_agreement_id(&signed_rca.agreement);

    // Early replay/idempotency check, ahead of the capacity cap and the IPFS
    // fetch: a recorded proposal's retry is resolved from the store, never
    // capacity-rejected or charged for the download. Failing open is best-effort.
    match rca_store.lookup(agreement_id).await {
        Ok(None) => {}
        Ok(Some(prior)) => {
            // A different payload reusing the same id is the only true conflict.
            if prior.signed_payload != rca_bytes {
                tracing::warn!(
                    %agreement_id,
                    "replay conflict: agreement id re-sent with a different payload"
                );
                return Err(DipsError::ReplayConflict { agreement_id });
            }
            // Byte-identical re-send: return the stored outcome, skipping the fetch.
            // A rejected proposal stays rejected; any other status
            // (pending/accepted/completed/future) resolves to an accept.
            if prior.status == crate::store::STATUS_REJECTED {
                return Err(DipsError::ReplayRejected { agreement_id });
            }
            return Ok(agreement_id);
        }
        Err(error) => {
            tracing::warn!(%agreement_id, %error, "replay lookup failed, continuing validation");
        }
    }

    // Capacity safeguard: cap live agreements (pending or accepted) per rolling
    // 24h, checked before the IPFS fetch so an over-cap proposal costs no download.
    // Count-then-store isn't atomic, so concurrent proposals may overshoot slightly.
    if let Some(limit) = max_new_agreements_per_24h {
        match rca_store.count_since(CAPACITY_WINDOW).await {
            Ok(count) if count >= *limit => {
                return Err(DipsError::CapacityExceeded { limit: *limit });
            }
            Ok(_) => {}
            // Fail open: the cap is a best-effort safeguard, not a security control,
            // so a transient count failure shouldn't reject a valid proposal.
            Err(e) => tracing::warn!(error = %e, "DIPs capacity check failed; allowing proposal"),
        }
    }

    // Decode metadata
    let metadata =
        AcceptIndexingAgreementMetadata::abi_decode(signed_rca.agreement.metadata.as_ref())
            .map_err(|e| {
                DipsError::AbiDecoding(format!(
                    "Failed to decode AcceptIndexingAgreementMetadata: {e}"
                ))
            })?;

    // Only support V1 terms (IndexingAgreementVersion.V1 = 0 in Solidity enum)
    if metadata.version != 0 {
        return Err(DipsError::UnsupportedMetadataVersion(metadata.version));
    }

    // Decode terms
    let terms = IndexingAgreementTermsV1::abi_decode(metadata.terms.as_ref()).map_err(|e| {
        DipsError::AbiDecoding(format!("Failed to decode IndexingAgreementTermsV1: {e}"))
    })?;

    // Convert bytes32 deployment ID to IPFS hash
    let deployment_id = bytes32_to_ipfs_hash(&metadata.subgraphDeploymentId.0);

    // Fetch IPFS manifest
    let manifest = ipfs_fetcher.fetch(&deployment_id).await?;

    // Get network from manifest; an empty network field is a malformed manifest.
    let network_name = manifest
        .network()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| DipsError::InvalidSubgraphManifest(deployment_id.clone()))?;

    // Reject networks this indexer hasn't configured for DIPs at the manifest step,
    // instead of relying on the price lookup to miss the network later.
    if !price_calculator.is_supported(network_name) {
        tracing::info!(
            agreement_id = %agreement_id,
            network = %network_name,
            deployment_id = %deployment_id,
            "network not in configured supported_networks, rejecting proposal"
        );
        return Err(DipsError::UnsupportedNetwork(network_name.to_string()));
    }

    // Resolve chain ID for logging context
    let chain_id = registry
        .get_network_by_id(network_name)
        .map(|n| n.caip2_id.to_string())
        .or_else(|| additional_networks.get(network_name).cloned())
        .unwrap_or_else(|| "unknown".to_string());

    // Validate price minimums
    let offered_tokens_per_second = terms.tokensPerSecond;
    match price_calculator.get_minimum_price(network_name) {
        Some(price) if offered_tokens_per_second.lt(&Uint::from(price)) => {
            tracing::info!(
                agreement_id = %agreement_id,
                network = %network_name,
                chain_id = %chain_id,
                deployment_id = %deployment_id,
                offered = %offered_tokens_per_second,
                minimum = %price,
                "tokens_per_second below minimum, rejecting proposal"
            );
            return Err(DipsError::TokensPerSecondTooLow {
                network: network_name.to_string(),
                minimum: price,
                offered: offered_tokens_per_second,
            });
        }
        Some(_) => {}
        None => {
            tracing::info!(
                agreement_id = %agreement_id,
                network = %network_name,
                chain_id = %chain_id,
                deployment_id = %deployment_id,
                "network not configured in price calculator, rejecting proposal"
            );
            return Err(DipsError::UnsupportedNetwork(network_name.to_string()));
        }
    }

    // Validate entity price minimum
    let offered_entity_price = terms.tokensPerEntityPerSecond;
    if offered_entity_price < price_calculator.entity_price() {
        tracing::info!(
            agreement_id = %agreement_id,
            network = %network_name,
            chain_id = %chain_id,
            deployment_id = %deployment_id,
            offered = %offered_entity_price,
            minimum = %price_calculator.entity_price(),
            "tokens_per_entity_per_second below minimum, rejecting proposal"
        );
        return Err(DipsError::TokensPerEntityPerSecondTooLow {
            minimum: price_calculator.entity_price(),
            offered: offered_entity_price,
        });
    }

    tracing::debug!(
        agreement_id = %agreement_id,
        network = %network_name,
        deployment_id = %deployment_id,
        "creating RCA agreement"
    );

    // Store the raw signed RCA bytes
    rca_store
        .store_rca(agreement_id, rca_bytes, PROTOCOL_VERSION)
        .await
        .map_err(|error| {
            tracing::error!(%agreement_id, %error, "failed to store RCA");
            error
        })?;

    Ok(agreement_id)
}

#[cfg(test)]
mod test {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    use crate::{
        derive_agreement_id,
        ipfs::{EmptyNetworkIpfsFetcher, FailingIpfsFetcher, MockIpfsFetcher},
        price::PriceCalculator,
        rca_eip712_domain,
        server::DipsServerContext,
        store::{FailingRcaStore, InMemoryRcaStore, LookupFailsStore, RcaStore},
        trusted_signers::{StaticTrustedSigners, TrustedSignerSource},
        AcceptIndexingAgreementMetadata, DipsError, IndexingAgreementTermsV1,
        RecurringCollectionAgreement, SignedRecurringCollectionAgreement,
    };
    use thegraph_core::alloy::{
        primitives::{keccak256, Address, FixedBytes, U256},
        signers::local::PrivateKeySigner,
        sol_types::{Eip712Domain, SolValue},
    };

    // Fixed signer and domain for round-tripping RCA signatures in tests. The
    // recovered address equals `test_signer().address()`; the domain must match
    // the one stored in the test context so recovery succeeds.
    fn test_signer() -> PrivateKeySigner {
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse()
            .unwrap()
    }

    fn test_rca_domain() -> Eip712Domain {
        rca_eip712_domain(1337, Address::repeat_byte(0xCC))
    }

    fn trusted_signers_for_test() -> Arc<dyn TrustedSignerSource> {
        Arc::new(StaticTrustedSigners(HashSet::from([
            test_signer().address()
        ])))
    }

    fn create_test_context() -> Arc<DipsServerContext> {
        Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        })
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_untrusted_signer_rejected() {
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        // Trusted set excludes the test signer, so a valid signature is rejected.
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: Arc::new(StaticTrustedSigners::default()),
            max_new_agreements_per_24h: None,
        });
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::SenderNotTrusted { .. })));
    }

    /// Sign an RCA with the fixed test key over the test domain and ABI-encode
    /// the wrapper, producing the wire bytes `validate_and_create_rca` decodes
    /// and recovers the signer from.
    fn rca_to_wire_bytes(rca: RecurringCollectionAgreement) -> Vec<u8> {
        rca.sign(&test_rca_domain(), test_signer())
            .expect("signing test RCA")
            .abi_encode()
    }

    fn create_test_rca(
        payer: Address,
        service_provider: Address,
        tokens_per_second: U256,
        tokens_per_entity_per_second: U256,
    ) -> RecurringCollectionAgreement {
        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: tokens_per_second,
            tokensPerEntityPerSecond: tokens_per_entity_per_second,
        };

        let metadata = AcceptIndexingAgreementMetadata {
            // Any bytes32 works - MockIpfsFetcher ignores the deployment ID
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 0, // IndexingAgreementVersion.V1 = 0
            terms: terms.abi_encode().into(),
        };

        RecurringCollectionAgreement {
            deadline: u64::MAX,
            endsAt: u64::MAX,
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            conditions: 0,
            nonce: U256::from(1),
            metadata: metadata.abi_encode().into(),
        }
    }

    #[test]
    fn test_derive_agreement_id() {
        let rca = RecurringCollectionAgreement {
            deadline: 1000,
            endsAt: 2000,
            payer: Address::repeat_byte(0x01),
            dataService: Address::repeat_byte(0x02),
            serviceProvider: Address::repeat_byte(0x03),
            maxInitialTokens: U256::from(100),
            maxOngoingTokensPerSecond: U256::from(10),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            conditions: 0,
            nonce: U256::from(42),
            metadata: Default::default(),
        };

        let id = derive_agreement_id(&rca);

        // Verify against the on-chain formula:
        // bytes16(keccak256(abi.encode(payer, dataService, serviceProvider, deadline, nonce)))
        let expected_hash = keccak256(
            (
                rca.payer,
                rca.dataService,
                rca.serviceProvider,
                rca.deadline,
                rca.nonce,
            )
                .abi_encode(),
        );
        assert_eq!(id.as_bytes(), &expected_hash[..16]);
    }

    /// Shared test vector with dipper (dipper-rpc/src/indexer.rs).
    /// Both repos must produce the same bytes16 for this input.
    /// If this test fails, the derivation has drifted from the on-chain
    /// contract and/or from dipper -- cancellations and agreement
    /// matching will break silently.
    #[test]
    fn test_derive_agreement_id_shared_vector() {
        let rca = RecurringCollectionAgreement {
            deadline: 1700000300,
            endsAt: 1700086400,
            payer: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
                .parse()
                .unwrap(),
            dataService: "0xCf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9"
                .parse()
                .unwrap(),
            serviceProvider: "0xf4EF6650E48d099a4972ea5B414daB86e1998Bd3"
                .parse()
                .unwrap(),
            maxInitialTokens: U256::from(1_000_000_000_000_000_000u64),
            maxOngoingTokensPerSecond: U256::from(1_000_000_000_000_000u64),
            minSecondsPerCollection: 3600,
            maxSecondsPerCollection: 86400,
            conditions: 0,
            nonce: U256::from(0x019d44a86ac97e938672e2501fe630f2u128),
            metadata: Default::default(),
        };

        let id = derive_agreement_id(&rca);

        // Pinned expected value. If this fails, check:
        // 1. dipper: dipper-rpc/src/indexer.rs test_derive_agreement_id_shared_vector
        // 2. Solidity: RecurringCollector._generateAgreementId()
        let expected: [u8; 16] = [
            0x55, 0x79, 0x42, 0xae, 0xfa, 0xb6, 0x16, 0x09, 0xcf, 0xb9, 0xee, 0x14, 0xd3, 0x09,
            0xa1, 0x7e,
        ];
        assert_eq!(
            id.as_bytes(),
            &expected,
            "derive_agreement_id output does not match pinned shared vector. \
             Actual: 0x{} -- update this test AND the matching test in \
             dipper (dipper-rpc/src/indexer.rs)",
            id.as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }

    /// Guards against drift in the sol! struct layout for the audit-branch
    /// `conditions` field. If `conditions` were ever moved, renamed, or
    /// dropped from the decoder, this round-trip would either fail to
    /// decode or return a corrupted value in the field's slot.
    #[test]
    fn test_rca_conditions_field_roundtrip() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let mut rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        rca.conditions = 0xABCD; // arbitrary non-zero 16-bit value

        let encoded = rca_to_wire_bytes(rca.clone());
        let decoded = SignedRecurringCollectionAgreement::abi_decode(encoded.as_ref())
            .expect("roundtrip decode failed");

        assert_eq!(
            decoded.agreement.conditions, 0xABCD,
            "conditions field did not survive ABI round-trip"
        );
        // Cross-check surrounding fields are intact, so a failure of the
        // conditions field isn't silently misread from a neighbour's slot.
        assert_eq!(
            decoded.agreement.maxSecondsPerCollection,
            rca.maxSecondsPerCollection
        );
        assert_eq!(decoded.agreement.nonce, rca.nonce);
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_success() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result =
            super::validate_and_create_rca(ctx.clone(), &service_provider, rca_bytes).await;

        assert!(result.is_ok(), "got: {:?}", result);
        assert_eq!(result.unwrap(), agreement_id);

        // Verify it was stored
        let store = ctx.rca_store.as_ref();
        let in_memory = store.as_any().downcast_ref::<InMemoryRcaStore>().unwrap();
        let data = in_memory.data.read().await;
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].0, agreement_id);
    }

    #[test]
    fn test_recover_signer_recovers_the_signing_key() {
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            Address::repeat_byte(0x11),
            U256::from(200),
            U256::from(100),
        );

        let signed = rca
            .sign(&test_rca_domain(), test_signer())
            .expect("signing test RCA");
        let recovered = signed
            .recover_signer(&test_rca_domain())
            .expect("recovering signer");

        assert_eq!(recovered, test_signer().address());
    }

    #[test]
    fn test_recover_signer_rejects_empty_signature() {
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            Address::repeat_byte(0x11),
            U256::from(200),
            U256::from(100),
        );
        let signed = SignedRecurringCollectionAgreement {
            agreement: rca,
            signature: Default::default(),
        };

        assert!(matches!(
            signed.recover_signer(&test_rca_domain()),
            Err(DipsError::InvalidSignature(_))
        ));
    }

    #[test]
    fn test_recover_signer_rejects_malformed_signature() {
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            Address::repeat_byte(0x11),
            U256::from(200),
            U256::from(100),
        );
        let signed = SignedRecurringCollectionAgreement {
            agreement: rca,
            signature: vec![0xAA; 10].into(),
        };

        assert!(matches!(
            signed.recover_signer(&test_rca_domain()),
            Err(DipsError::InvalidSignature(_))
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_missing_signature_rejected() {
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );
        // An unsigned proposal must be rejected before any other validation.
        let rca_bytes = SignedRecurringCollectionAgreement {
            agreement: rca,
            signature: Default::default(),
        }
        .abi_encode();

        let ctx = create_test_context();
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::InvalidSignature(_))));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_wrong_service_provider() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let wrong_service_provider = Address::repeat_byte(0x99);

        let rca = create_test_rca(
            payer,
            wrong_service_provider,
            U256::from(200),
            U256::from(100),
        );

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::UnexpectedServiceProvider { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_tokens_per_second_too_low() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        // Offer 50, minimum is 100
        let rca = create_test_rca(payer, service_provider, U256::from(50), U256::from(100));

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::TokensPerSecondTooLow { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_entity_price_too_low() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        // Offer 200 tokens/sec (ok), but only 10 entity price (minimum is 50)
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(10));

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::TokensPerEntityPerSecondTooLow { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_unsupported_network() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        // Create context with IPFS fetcher returning unsupported network
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher {
                network: "unsupported-network".to_string(),
            }),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        let rca_bytes = rca_to_wire_bytes(rca);
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::UnsupportedNetwork(_))));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_invalid_metadata_version() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: U256::from(200),
            tokensPerEntityPerSecond: U256::from(100),
        };

        // Use version 2 (unsupported)
        let metadata = AcceptIndexingAgreementMetadata {
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 2, // Unsupported version
            terms: terms.abi_encode().into(),
        };

        let rca = RecurringCollectionAgreement {
            deadline: u64::MAX,
            endsAt: u64::MAX,
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            conditions: 0,
            nonce: U256::from(1),
            metadata: metadata.abi_encode().into(),
        };

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::UnsupportedMetadataVersion(2))
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_deadline_expired() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: U256::from(200),
            tokensPerEntityPerSecond: U256::from(100),
        };

        let metadata = AcceptIndexingAgreementMetadata {
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 0, // IndexingAgreementVersion.V1 = 0
            terms: terms.abi_encode().into(),
        };

        // Set deadline to the past
        let rca = RecurringCollectionAgreement {
            deadline: 1, // 1 second after epoch - definitely in the past
            endsAt: u64::MAX,
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            conditions: 0,
            nonce: U256::from(1),
            metadata: metadata.abi_encode().into(),
        };

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::DeadlineExpired { .. })));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_agreement_expired() {
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: U256::from(200),
            tokensPerEntityPerSecond: U256::from(100),
        };

        let metadata = AcceptIndexingAgreementMetadata {
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 0, // IndexingAgreementVersion.V1 = 0
            terms: terms.abi_encode().into(),
        };

        // Set endsAt to the past
        let rca = RecurringCollectionAgreement {
            deadline: u64::MAX,
            endsAt: 1, // 1 second after epoch - definitely in the past
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            conditions: 0,
            nonce: U256::from(1),
            metadata: metadata.abi_encode().into(),
        };

        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::AgreementExpired { .. })));
    }

    // =========================================================================
    // Additional tests for complete coverage (following test-arrange-act-assert)
    // =========================================================================

    #[tokio::test]
    async fn test_validate_and_create_rca_malformed_abi() {
        // Arrange
        let service_provider = Address::repeat_byte(0x11);
        let ctx = create_test_context();

        let malformed_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF]; // Not valid ABI

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, malformed_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::AbiDecoding(_))),
            "Expected AbiDecoding error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_ipfs_failure() {
        // Arrange
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        // Context with failing IPFS fetcher
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(FailingIpfsFetcher),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        let rca_bytes = rca_to_wire_bytes(rca);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::SubgraphManifestUnavailable(_))),
            "Expected SubgraphManifestUnavailable error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_manifest_no_network() {
        // Arrange
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        // Context with IPFS fetcher returning manifest without network
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::no_network()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        let rca_bytes = rca_to_wire_bytes(rca);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::InvalidSubgraphManifest(_))),
            "Expected InvalidSubgraphManifest error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_empty_network() {
        // Arrange
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        // Context with a manifest whose data source has an empty network field
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(EmptyNetworkIpfsFetcher),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        let rca_bytes = rca_to_wire_bytes(rca);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::InvalidSubgraphManifest(_))),
            "Expected InvalidSubgraphManifest for empty network, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_store_failure() {
        // Arrange
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        // Context with failing store
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(FailingRcaStore),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        let rca_bytes = rca_to_wire_bytes(rca);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::UnknownError(_))),
            "Expected UnknownError from store failure, got: {:?}",
            result
        );
    }

    // =========================================================================
    // Unit tests for helper functions
    // =========================================================================

    #[test]
    fn test_bytes32_to_ipfs_hash_format() {
        // Arrange
        let bytes: [u8; 32] = [0xAB; 32];

        // Act
        let hash = super::bytes32_to_ipfs_hash(&bytes);

        // Assert - CIDv0 format starts with "Qm" and is 46 characters
        assert!(
            hash.starts_with("Qm"),
            "IPFS CIDv0 should start with 'Qm', got: {}",
            hash
        );
        assert_eq!(
            hash.len(),
            46,
            "IPFS CIDv0 should be 46 characters, got: {}",
            hash.len()
        );
    }

    #[test]
    fn test_bytes32_to_ipfs_hash_deterministic() {
        // Arrange
        let bytes: [u8; 32] = [0x12; 32];

        // Act
        let hash1 = super::bytes32_to_ipfs_hash(&bytes);
        let hash2 = super::bytes32_to_ipfs_hash(&bytes);

        // Assert
        assert_eq!(hash1, hash2, "Same input should produce same output");
    }

    #[test]
    fn test_bytes32_to_ipfs_hash_different_inputs() {
        // Arrange
        let bytes1: [u8; 32] = [0x00; 32];
        let bytes2: [u8; 32] = [0xFF; 32];

        // Act
        let hash1 = super::bytes32_to_ipfs_hash(&bytes1);
        let hash2 = super::bytes32_to_ipfs_hash(&bytes2);

        // Assert
        assert_ne!(
            hash1, hash2,
            "Different inputs should produce different outputs"
        );
    }

    #[test]
    fn test_bytes32_to_ipfs_hash_known_vector() {
        // Arrange - all zeros should produce a known hash
        // Multihash: 0x12 (sha256) + 0x20 (32 bytes) + 32 zero bytes
        // Base58 encoding of [0x12, 0x20, 0x00 * 32]
        let bytes: [u8; 32] = [0x00; 32];

        // Act
        let hash = super::bytes32_to_ipfs_hash(&bytes);

        // Assert - verified by manual calculation
        // The multihash [0x12, 0x20, 0, 0, ...] encodes to this CIDv0
        assert_eq!(
            hash, "QmNLei78zWmzUdbeRB3CiUfAizWUrbeeZh5K1rhAQKCh51",
            "Known test vector mismatch"
        );
    }

    // =========================================================================
    // Early replay / idempotency check (before the IPFS fetch)
    // =========================================================================

    use crate::{ipfs::GraphManifest, ipfs::IpfsFetcher, PROTOCOL_VERSION};

    /// IPFS fetcher that fails the test if reached. Proves the replay
    /// short-circuit returns before any manifest download.
    #[derive(Debug)]
    struct PanicIpfsFetcher;

    #[async_trait::async_trait]
    impl IpfsFetcher for PanicIpfsFetcher {
        async fn fetch(&self, _file: &str) -> Result<GraphManifest, DipsError> {
            panic!("IPFS fetch must not run on the replay short-circuit path");
        }
    }

    /// Build a context whose store is the supplied seeded one and whose IPFS
    /// fetcher panics if called, so a reached download surfaces as a test panic.
    fn context_with_store_and_panic_fetcher(
        store: Arc<InMemoryRcaStore>,
    ) -> Arc<DipsServerContext> {
        Arc::new(DipsServerContext {
            rca_store: store,
            ipfs_fetcher: Arc::new(PanicIpfsFetcher),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        })
    }

    /// Overwrite the status of the single seeded row, for the accepted/rejected cases.
    async fn set_status(store: &InMemoryRcaStore, status: &str) {
        let mut data = store.data.write().await;
        data[0].3 = status.to_string();
    }

    #[tokio::test]
    async fn test_replay_first_time_runs_full_pipeline() {
        // Arrange: empty store, real mock fetcher; this is the non-replay path.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let ctx = create_test_context();
        let rca_bytes = rca_to_wire_bytes(rca);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert_eq!(result.unwrap(), agreement_id);
    }

    #[tokio::test]
    async fn test_replay_identical_pending_accepts_without_fetch() {
        // Arrange: seed the identical bytes as a pending row.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, rca_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        let ctx = context_with_store_and_panic_fetcher(store);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert: replays the prior accept, panicking fetcher proves no download.
        assert_eq!(result.unwrap(), agreement_id);
    }

    #[tokio::test]
    async fn test_replay_identical_accepted_accepts_without_fetch() {
        // Arrange: seed identical bytes, then promote the row to accepted.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, rca_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        set_status(&store, "accepted").await;
        let ctx = context_with_store_and_panic_fetcher(store);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert_eq!(result.unwrap(), agreement_id);
    }

    #[tokio::test]
    async fn test_replay_identical_completed_accepts_without_fetch() {
        // Arrange: seed identical bytes, then mark the row completed (the status
        // the agent sets when an agreement goes live on-chain and is retired).
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, rca_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        set_status(&store, "completed").await;
        let ctx = context_with_store_and_panic_fetcher(store);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert: a completed proposal replays Ok, not a ReplayConflict.
        assert_eq!(result.unwrap(), agreement_id);
    }

    #[tokio::test]
    async fn test_replay_identical_rejected_rerejects_without_fetch() {
        // Arrange: seed identical bytes, then mark the row rejected.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, rca_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        set_status(&store, "rejected").await;
        let ctx = context_with_store_and_panic_fetcher(store);

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert
        assert!(matches!(result, Err(DipsError::ReplayRejected { .. })));
    }

    #[tokio::test]
    async fn test_replay_same_id_different_bytes_conflicts_without_fetch() {
        // Arrange: seed one payload, then submit a different payload sharing the id.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);

        // Same id preimage (payer, dataService, serviceProvider, deadline, nonce),
        // different terms so the wire bytes differ.
        let seeded = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let conflicting =
            create_test_rca(payer, service_provider, U256::from(999), U256::from(100));
        let agreement_id = derive_agreement_id(&seeded);
        assert_eq!(agreement_id, derive_agreement_id(&conflicting));

        let seeded_bytes = rca_to_wire_bytes(seeded);
        let conflicting_bytes = rca_to_wire_bytes(conflicting);
        assert_ne!(seeded_bytes, conflicting_bytes);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, seeded_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        let ctx = context_with_store_and_panic_fetcher(store.clone());

        // Act
        let result =
            super::validate_and_create_rca(ctx, &service_provider, conflicting_bytes).await;

        // Assert: conflict reported and the stored row is left unchanged.
        assert!(matches!(result, Err(DipsError::ReplayConflict { .. })));
        let stored = store.lookup(agreement_id).await.unwrap().unwrap();
        assert_eq!(stored.signed_payload, seeded_bytes);
    }

    #[tokio::test]
    async fn test_replay_lookup_error_fails_open_and_validates() {
        // Arrange: a store whose lookup errors but whose write succeeds, paired
        // with a real fetcher so validation can run to completion.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(LookupFailsStore),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: None,
        });

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert: the swallowed lookup error still lets validation run to an
        // accept, rather than surfacing as a rejection.
        assert_eq!(result.unwrap(), agreement_id);
    }

    #[tokio::test]
    async fn test_replay_accepts_even_when_at_capacity() {
        // Arrange: a recorded pending proposal with the per-24h cap already at
        // one, so the capacity check alone would reject. The panicking fetcher
        // proves the replay short-circuits before any manifest download.
        let payer = Address::repeat_byte(0x42);
        let service_provider = Address::repeat_byte(0x11);
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = derive_agreement_id(&rca);
        let rca_bytes = rca_to_wire_bytes(rca);

        let store = Arc::new(InMemoryRcaStore::default());
        store
            .store_rca(agreement_id, rca_bytes.clone(), PROTOCOL_VERSION)
            .await
            .unwrap();
        let ctx = Arc::new(DipsServerContext {
            rca_store: store,
            ipfs_fetcher: Arc::new(PanicIpfsFetcher),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: Some(1),
        });

        // Act
        let result = super::validate_and_create_rca(ctx, &service_provider, rca_bytes).await;

        // Assert: the prior accept comes back rather than CapacityExceeded.
        assert_eq!(result.unwrap(), agreement_id);
    }

    /// Build a context backed by `store` with the per-24h cap set to `max`.
    fn capacity_ctx(store: Arc<dyn RcaStore>, max: Option<u64>) -> Arc<DipsServerContext> {
        Arc::new(DipsServerContext {
            rca_store: store,
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
            rca_domain: test_rca_domain(),
            trusted_signers: trusted_signers_for_test(),
            max_new_agreements_per_24h: max,
        })
    }

    /// Fill an in-memory store with `n` distinct stored proposals.
    async fn prefill(store: &InMemoryRcaStore, n: u64) {
        for i in 0..n {
            store
                .store_rca(uuid::Uuid::from_u128(i as u128 + 1), vec![i as u8], 2)
                .await
                .unwrap();
        }
    }

    /// A store whose count query fails but whose writes succeed, to exercise the
    /// capacity check's fail-open path.
    #[derive(Debug, Default)]
    struct CountFailingStore(InMemoryRcaStore);

    #[async_trait::async_trait]
    impl RcaStore for CountFailingStore {
        async fn store_rca(
            &self,
            agreement_id: uuid::Uuid,
            signed_rca: Vec<u8>,
            version: u64,
        ) -> Result<(), DipsError> {
            self.0.store_rca(agreement_id, signed_rca, version).await
        }

        async fn lookup(
            &self,
            agreement_id: uuid::Uuid,
        ) -> Result<Option<crate::store::StoredProposal>, DipsError> {
            self.0.lookup(agreement_id).await
        }

        async fn count_since(&self, _window: std::time::Duration) -> Result<u64, DipsError> {
            Err(DipsError::UnknownError(anyhow::anyhow!(
                "count failed (test)"
            )))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn capacity_at_limit_rejects() {
        let service_provider = Address::repeat_byte(0x11);
        let store = Arc::new(InMemoryRcaStore::default());
        prefill(&store, 2).await;
        let ctx = capacity_ctx(store, Some(2));
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        let result =
            super::validate_and_create_rca(ctx.clone(), &service_provider, rca_to_wire_bytes(rca))
                .await;

        assert!(matches!(
            result,
            Err(DipsError::CapacityExceeded { limit: 2 })
        ));
        // The over-cap proposal must not have been stored.
        let stored = ctx
            .rca_store
            .as_any()
            .downcast_ref::<InMemoryRcaStore>()
            .unwrap();
        assert_eq!(stored.data.read().await.len(), 2);
    }

    #[tokio::test]
    async fn capacity_under_limit_passes() {
        let service_provider = Address::repeat_byte(0x11);
        let store = Arc::new(InMemoryRcaStore::default());
        prefill(&store, 1).await;
        let ctx = capacity_ctx(store, Some(2));
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        let result =
            super::validate_and_create_rca(ctx.clone(), &service_provider, rca_to_wire_bytes(rca))
                .await;

        assert!(result.is_ok(), "got: {:?}", result);
        // The accepted proposal is stored, taking the count from 1 to 2.
        let stored = ctx
            .rca_store
            .as_any()
            .downcast_ref::<InMemoryRcaStore>()
            .unwrap();
        assert_eq!(stored.data.read().await.len(), 2);
    }

    #[tokio::test]
    async fn capacity_unset_skips_check() {
        let service_provider = Address::repeat_byte(0x11);
        let store = Arc::new(InMemoryRcaStore::default());
        // Five already stored, but no cap configured, so a new one still passes.
        prefill(&store, 5).await;
        let ctx = capacity_ctx(store, None);
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        let result =
            super::validate_and_create_rca(ctx, &service_provider, rca_to_wire_bytes(rca)).await;

        assert!(result.is_ok(), "got: {:?}", result);
    }

    #[tokio::test]
    async fn capacity_count_failure_fails_open() {
        let service_provider = Address::repeat_byte(0x11);
        // The count query errors; the cap is best-effort, so the proposal is
        // allowed through rather than rejected.
        let ctx = capacity_ctx(Arc::new(CountFailingStore::default()), Some(1));
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        let result =
            super::validate_and_create_rca(ctx.clone(), &service_provider, rca_to_wire_bytes(rca))
                .await;

        assert!(result.is_ok(), "got: {:?}", result);
        // Fail-open still completes the path and stores the proposal.
        let stored = ctx
            .rca_store
            .as_any()
            .downcast_ref::<CountFailingStore>()
            .unwrap();
        assert_eq!(stored.0.data.read().await.len(), 1);
    }

    #[tokio::test]
    async fn capacity_zero_limit_rejects_all() {
        let service_provider = Address::repeat_byte(0x11);
        // An empty store with a cap of 0 still rejects every proposal (0 >= 0).
        let ctx = capacity_ctx(Arc::new(InMemoryRcaStore::default()), Some(0));
        let rca = create_test_rca(
            Address::repeat_byte(0x42),
            service_provider,
            U256::from(200),
            U256::from(100),
        );

        let result =
            super::validate_and_create_rca(ctx, &service_provider, rca_to_wire_bytes(rca)).await;

        assert!(matches!(
            result,
            Err(DipsError::CapacityExceeded { limit: 0 })
        ));
    }
}
