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
//! 3. **IPFS manifest** - Subgraph deployment exists and is parseable
//! 4. **Network** - Subgraph's network is supported by this indexer
//! 5. **Pricing** - Offered price meets indexer's minimum
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
    primitives::{keccak256, ruint::aliases::U256, Uint},
    signers::SignerSync,
    sol,
    sol_types::{Eip712Domain, SolValue},
};

#[cfg(feature = "db")]
pub mod database;
pub mod inflight;
pub mod ipfs;
pub mod price;
#[cfg(feature = "rpc")]
pub mod proto;
#[cfg(test)]
mod registry;
#[cfg(feature = "rpc")]
pub mod server;
pub mod store;

use thiserror::Error;
use uuid::Uuid;

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

#[derive(Error, Debug)]
pub enum DipsError {
    // RCA validation
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

/// Validate and create a RecurringCollectionAgreement.
///
/// Performs validation:
/// - Service provider match
/// - Deadline and expiry checks
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
        ..
    } = ctx.as_ref();

    // Decode SignedRCA
    let signed_rca = SignedRecurringCollectionAgreement::abi_decode(rca_bytes.as_ref())
        .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

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
        server::DipsServerContext,
        store::{FailingRcaStore, InMemoryRcaStore},
        AcceptIndexingAgreementMetadata, DipsError, IndexingAgreementTermsV1,
        RecurringCollectionAgreement, SignedRecurringCollectionAgreement,
    };
    use thegraph_core::alloy::{
        primitives::{keccak256, Address, FixedBytes, U256},
        sol_types::SolValue,
    };

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
        })
    }

    /// Helper: encode an RCA as a `SignedRecurringCollectionAgreement`
    /// ABI payload. The signature field is no longer consumed by the
    /// validator; this just produces the wire bytes that
    /// `validate_and_create_rca` expects to decode.
    fn rca_to_wire_bytes(rca: RecurringCollectionAgreement) -> Vec<u8> {
        SignedRecurringCollectionAgreement {
            agreement: rca,
            signature: Default::default(),
        }
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
}
