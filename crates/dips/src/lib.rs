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
//! 1. **Signature** - EIP-712 signature recovers to an authorized signer
//! 2. **Signer authorization** - Signer is authorized for the payer (via escrow accounts)
//! 3. **Service provider** - RCA is addressed to this indexer
//! 4. **Timestamps** - Deadline and end time haven't passed
//! 5. **IPFS manifest** - Subgraph deployment exists and is parseable
//! 6. **Network** - Subgraph's network is supported by this indexer
//! 7. **Pricing** - Offered price meets indexer's minimum
//!
//! # Trust Model
//!
//! Payers deposit funds into the PaymentsEscrow contract and authorize signers.
//! The escrow has a **thawing period** for withdrawals, giving indexers time to
//! collect owed fees before funds can be withdrawn. This crate checks signer
//! authorization via the network subgraph, which may lag chain state slightly.
//! The thawing period protects against this lag.
//!
//! # Modules
//!
//! - [`server`] - gRPC server handling RCA proposals
//! - [`store`] - Storage trait for RCA proposals
//! - [`database`] - PostgreSQL implementation
//! - [`signers`] - Signer authorization via escrow accounts
//! - [`ipfs`] - IPFS client for subgraph manifests
//! - [`price`] - Minimum price enforcement

use std::{str::FromStr, sync::Arc};

use server::DipsServerContext;
use thegraph_core::alloy::{
    core::primitives::Address,
    primitives::{ruint::aliases::U256, ChainId, Signature, Uint},
    signers::SignerSync,
    sol,
    sol_types::{eip712_domain, Eip712Domain, SolStruct, SolValue},
};

#[cfg(feature = "db")]
pub mod database;
pub mod ipfs;
pub mod price;
#[cfg(feature = "rpc")]
pub mod proto;
#[cfg(test)]
mod registry;
#[cfg(feature = "rpc")]
pub mod server;
pub mod signers;
pub mod store;

use thiserror::Error;
use uuid::Uuid;

/// Protocol version (seconds-based RCA)
pub const PROTOCOL_VERSION: u64 = 2;

/// Create an EIP-712 domain for RecurringCollectionAgreement.
///
/// Used to sign `RecurringCollectionAgreement` messages. The `verifying_contract`
/// is the deployed RecurringCollector address.
pub fn rca_eip712_domain(chain_id: ChainId, recurring_collector: Address) -> Eip712Domain {
    eip712_domain! {
        name: "RecurringCollector",
        version: "1",
        chain_id: chain_id,
        verifying_contract: recurring_collector,
    }
}

sol! {
    // === RCA Types (seconds-based RecurringCollectionAgreement) ===

    /// The on-chain RecurringCollectionAgreement type.
    ///
    /// Matches `IRecurringCollector.RecurringCollectionAgreement` exactly.
    #[derive(Debug, PartialEq)]
    struct RecurringCollectionAgreement {
        bytes16 agreementId;
        // NB: The on-chain struct declares these as uint64 for storage efficiency,
        // but the EIP-712 typehash uses uint256. We must match the typehash.
        uint256 deadline;
        uint256 endsAt;
        address payer;
        address dataService;
        address serviceProvider;
        uint256 maxInitialTokens;
        uint256 maxOngoingTokensPerSecond;
        uint32 minSecondsPerCollection;
        uint32 maxSecondsPerCollection;
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

    // === Cancellation ===

    #[derive(Debug, PartialEq)]
    struct SignedCancellationRequest {
        CancellationRequest request;
        bytes signature;
    }

    #[derive(Debug, PartialEq)]
    struct CancellationRequest {
        bytes16 agreement_id;
    }
}

#[derive(Error, Debug)]
pub enum DipsError {
    // RCA validation
    #[error("signature is not valid, error: {0}")]
    InvalidSignature(String),
    #[error("RCA service provider {actual} does not match the expected address {expected}")]
    UnexpectedServiceProvider { expected: Address, actual: Address },
    #[error("cannot get subgraph manifest for {0}")]
    SubgraphManifestUnavailable(String),
    #[error("invalid subgraph id {0}")]
    InvalidSubgraphManifest(String),
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
    #[error("signer {0} not authorised")]
    SignerNotAuthorised(Address),
    #[error("cancelled_by is expected to match the signer")]
    UnexpectedSigner,
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

impl CancellationRequest {
    pub fn sign<S: SignerSync>(
        &self,
        domain: &Eip712Domain,
        signer: S,
    ) -> anyhow::Result<SignedCancellationRequest> {
        let voucher = SignedCancellationRequest {
            request: self.clone(),
            signature: signer.sign_typed_data_sync(self, domain)?.as_bytes().into(),
        };

        Ok(voucher)
    }
}

impl SignedCancellationRequest {
    // TODO: Validate all values
    pub fn validate(
        &self,
        domain: &Eip712Domain,
        expected_signer: &Address,
    ) -> Result<(), DipsError> {
        let sig = Signature::from_str(&self.signature.to_string())
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        let signer = sig
            .recover_address_from_prehash(&self.request.eip712_signing_hash(domain))
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        if signer.ne(expected_signer) {
            return Err(DipsError::UnexpectedSigner);
        }

        Ok(())
    }
    pub fn encode_vec(&self) -> Vec<u8> {
        self.abi_encode()
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
    /// Validate the RCA signature and basic fields.
    ///
    /// Checks:
    /// - EIP-712 signature is valid and recovers to an authorized signer for the payer
    /// - Signer is authorized for the payer (via escrow accounts)
    /// - Service provider matches expected indexer address
    pub fn validate(
        &self,
        signer_validator: &Arc<dyn signers::SignerValidator>,
        domain: &Eip712Domain,
        expected_service_provider: &Address,
    ) -> Result<(), DipsError> {
        let sig = Signature::try_from(self.signature.as_ref())
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        let payer = self.agreement.payer;
        let signer = sig
            .recover_address_from_prehash(&self.agreement.eip712_signing_hash(domain))
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        signer_validator
            .validate(&payer, &signer)
            .map_err(|_| DipsError::SignerNotAuthorised(signer))?;

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

/// Validate and create a RecurringCollectionAgreement.
///
/// Performs validation:
/// - EIP-712 signature verification
/// - IPFS manifest fetching and network validation
/// - Price minimum enforcement
///
/// Returns the agreement ID if successful, stores in database.
pub async fn validate_and_create_rca(
    ctx: Arc<DipsServerContext>,
    domain: &Eip712Domain,
    expected_service_provider: &Address,
    rca_bytes: Vec<u8>,
) -> Result<Uuid, DipsError> {
    let DipsServerContext {
        rca_store,
        ipfs_fetcher,
        price_calculator,
        signer_validator,
        registry,
        additional_networks,
        ..
    } = ctx.as_ref();

    // Decode SignedRCA
    let signed_rca = SignedRecurringCollectionAgreement::abi_decode(rca_bytes.as_ref())
        .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

    // Validate signature and basic fields
    signed_rca.validate(signer_validator, domain, expected_service_provider)?;

    // Validate deadline hasn't passed
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();

    let deadline: u64 = signed_rca
        .agreement
        .deadline
        .try_into()
        .map_err(|_| DipsError::InvalidRca("deadline overflow".to_string()))?;
    if deadline < now {
        return Err(DipsError::DeadlineExpired { deadline, now });
    }

    // Validate agreement hasn't already expired
    let ends_at: u64 = signed_rca
        .agreement
        .endsAt
        .try_into()
        .map_err(|_| DipsError::InvalidRca("endsAt overflow".to_string()))?;
    if ends_at < now {
        return Err(DipsError::AgreementExpired { ends_at, now });
    }

    // Extract agreement ID
    let agreement_id = Uuid::from_bytes(signed_rca.agreement.agreementId.into());

    // Decode metadata
    let metadata =
        AcceptIndexingAgreementMetadata::abi_decode(signed_rca.agreement.metadata.as_ref())
            .map_err(|e| {
                DipsError::AbiDecoding(format!(
                    "Failed to decode AcceptIndexingAgreementMetadata: {e}"
                ))
            })?;

    // Only support version 1 terms for now
    if metadata.version != 1 {
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

    // Get network from manifest
    let network_name = manifest
        .network()
        .ok_or_else(|| DipsError::InvalidSubgraphManifest(deployment_id.clone()))?;

    // Validate network is supported
    let network_supported = registry.get_network_by_id(network_name).is_some()
        || additional_networks.contains_key(network_name);

    if !network_supported {
        return Err(DipsError::UnsupportedNetwork(network_name.to_string()));
    }

    // Validate price minimums
    let offered_tokens_per_second = terms.tokensPerSecond;
    match price_calculator.get_minimum_price(network_name) {
        Some(price) if offered_tokens_per_second.lt(&Uint::from(price)) => {
            tracing::info!(
                agreement_id = %agreement_id,
                network = %network_name,
                deployment_id = %deployment_id,
                "offered tokens_per_second '{}' is lower than minimum price '{}'",
                offered_tokens_per_second,
                price
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
                deployment_id = %deployment_id,
                "network '{}' is not configured in price calculator",
                network_name
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
            deployment_id = %deployment_id,
            "offered tokens_per_entity_per_second '{}' is lower than minimum price '{}'",
            offered_entity_price,
            price_calculator.entity_price()
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

    use thegraph_core::alloy::{
        primitives::{Address, FixedBytes, U256},
        signers::local::PrivateKeySigner,
        sol_types::SolValue,
    };
    use uuid::Uuid;

    use crate::{
        ipfs::{FailingIpfsFetcher, MockIpfsFetcher},
        price::PriceCalculator,
        rca_eip712_domain,
        server::DipsServerContext,
        signers::{NoopSignerValidator, RejectingSignerValidator},
        store::{FailingRcaStore, InMemoryRcaStore},
        AcceptIndexingAgreementMetadata, DipsError, IndexingAgreementTermsV1,
        RecurringCollectionAgreement,
    };

    const CHAIN_ID: u64 = 42161; // Arbitrum One

    fn create_test_context() -> Arc<DipsServerContext> {
        Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()), // Returns "mainnet"
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            signer_validator: Arc::new(NoopSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        })
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
            version: 1,
            terms: terms.abi_encode().into(),
        };

        RecurringCollectionAgreement {
            agreementId: Uuid::now_v7().as_bytes().into(),
            deadline: U256::from(u64::MAX),
            endsAt: U256::from(u64::MAX),
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            metadata: metadata.abi_encode().into(),
        }
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_success() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let agreement_id = Uuid::from_bytes(rca.agreementId.into());

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx.clone(), &domain, &service_provider, rca_bytes)
                .await;

        assert!(result.is_ok());
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
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let wrong_service_provider = Address::repeat_byte(0x99);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(
            payer,
            wrong_service_provider,
            U256::from(200),
            U256::from(100),
        );

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::UnexpectedServiceProvider { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_tokens_per_second_too_low() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        // Offer 50, minimum is 100
        let rca = create_test_rca(payer, service_provider, U256::from(50), U256::from(100));

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::TokensPerSecondTooLow { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_entity_price_too_low() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        // Offer 200 tokens/sec (ok), but only 10 entity price (minimum is 50)
        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(10));

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::TokensPerEntityPerSecondTooLow { .. })
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_unsupported_network() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

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
            signer_validator: Arc::new(NoopSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        });

        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::UnsupportedNetwork(_))));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_invalid_metadata_version() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

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
            agreementId: Uuid::now_v7().as_bytes().into(),
            deadline: U256::from(u64::MAX),
            endsAt: U256::from(u64::MAX),
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            metadata: metadata.abi_encode().into(),
        };

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(
            result,
            Err(DipsError::UnsupportedMetadataVersion(2))
        ));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_deadline_expired() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: U256::from(200),
            tokensPerEntityPerSecond: U256::from(100),
        };

        let metadata = AcceptIndexingAgreementMetadata {
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 1,
            terms: terms.abi_encode().into(),
        };

        // Set deadline to the past
        let rca = RecurringCollectionAgreement {
            agreementId: Uuid::now_v7().as_bytes().into(),
            deadline: U256::from(1), // 1 second after epoch - definitely in the past
            endsAt: U256::from(u64::MAX),
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            metadata: metadata.abi_encode().into(),
        };

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::DeadlineExpired { .. })));
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_agreement_expired() {
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let terms = IndexingAgreementTermsV1 {
            tokensPerSecond: U256::from(200),
            tokensPerEntityPerSecond: U256::from(100),
        };

        let metadata = AcceptIndexingAgreementMetadata {
            subgraphDeploymentId: FixedBytes::ZERO,
            version: 1,
            terms: terms.abi_encode().into(),
        };

        // Set endsAt to the past
        let rca = RecurringCollectionAgreement {
            agreementId: Uuid::now_v7().as_bytes().into(),
            deadline: U256::from(u64::MAX),
            endsAt: U256::from(1), // 1 second after epoch - definitely in the past
            payer,
            dataService: Address::ZERO,
            serviceProvider: service_provider,
            maxInitialTokens: U256::from(1000),
            maxOngoingTokensPerSecond: U256::from(100),
            minSecondsPerCollection: 60,
            maxSecondsPerCollection: 3600,
            metadata: metadata.abi_encode().into(),
        };

        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        let ctx = create_test_context();
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        assert!(matches!(result, Err(DipsError::AgreementExpired { .. })));
    }

    // =========================================================================
    // Additional tests for complete coverage (following test-arrange-act-assert)
    // =========================================================================

    #[tokio::test]
    async fn test_validate_and_create_rca_malformed_abi() {
        // Arrange
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);
        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let ctx = create_test_context();

        let malformed_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF]; // Not valid ABI

        // Act
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, malformed_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::AbiDecoding(_))),
            "Expected AbiDecoding error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_unauthorized_signer() {
        // Arrange
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        // Context with rejecting signer validator
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            signer_validator: Arc::new(RejectingSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        });

        // Act
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::SignerNotAuthorised(_))),
            "Expected SignerNotAuthorised error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_ipfs_failure() {
        // Arrange
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        // Context with failing IPFS fetcher
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(FailingIpfsFetcher),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            signer_validator: Arc::new(NoopSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        });

        // Act
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

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
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        // Context with IPFS fetcher returning manifest without network
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(InMemoryRcaStore::default()),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::no_network()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            signer_validator: Arc::new(NoopSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        });

        // Act
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

        // Assert
        assert!(
            matches!(result, Err(DipsError::InvalidSubgraphManifest(_))),
            "Expected InvalidSubgraphManifest error, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_and_create_rca_store_failure() {
        // Arrange
        let payer_signer = PrivateKeySigner::random();
        let payer = payer_signer.address();
        let service_provider = Address::repeat_byte(0x11);
        let recurring_collector = Address::repeat_byte(0x22);

        let rca = create_test_rca(payer, service_provider, U256::from(200), U256::from(100));
        let domain = rca_eip712_domain(CHAIN_ID, recurring_collector);
        let signed_rca = rca.sign(&domain, payer_signer).unwrap();
        let rca_bytes = signed_rca.abi_encode();

        // Context with failing store
        let ctx = Arc::new(DipsServerContext {
            rca_store: Arc::new(FailingRcaStore),
            ipfs_fetcher: Arc::new(MockIpfsFetcher::default()),
            price_calculator: Arc::new(PriceCalculator::new(
                HashSet::from(["mainnet".to_string()]),
                BTreeMap::from([("mainnet".to_string(), U256::from(100))]),
                U256::from(50),
            )),
            signer_validator: Arc::new(NoopSignerValidator),
            registry: Arc::new(crate::registry::test_registry()),
            additional_networks: Arc::new(BTreeMap::new()),
        });

        // Act
        let result =
            super::validate_and_create_rca(ctx, &domain, &service_provider, rca_bytes).await;

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
