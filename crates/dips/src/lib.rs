// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc};

use server::DipsServerContext;
use thegraph_core::alloy::{
    core::primitives::Address,
    primitives::{b256, ChainId, PrimitiveSignature as Signature, Uint, B256},
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
#[cfg(feature = "rpc")]
pub mod server;
pub mod signers;
pub mod store;

use store::AgreementStore;
use thiserror::Error;
use uuid::Uuid;

/// The Arbitrum One (mainnet) chain ID (eip155).
const CHAIN_ID_ARBITRUM_ONE: ChainId = 0xa4b1; // 42161

/// DIPs EIP-712 domain salt
const EIP712_DOMAIN_SALT: B256 =
    b256!("b4632c657c26dce5d4d7da1d65bda185b14ff8f905ddbb03ea0382ed06c5ef28");

/// Create an EIP-712 domain given a chain ID and dispute manager address.
pub fn dips_agreement_eip712_domain() -> Eip712Domain {
    eip712_domain! {
        name: "Graph Protocol Indexing Agreement",
        version: "0",
        chain_id: CHAIN_ID_ARBITRUM_ONE,
        salt: EIP712_DOMAIN_SALT,
    }
}

pub fn dips_cancellation_eip712_domain() -> Eip712Domain {
    eip712_domain! {
        name: "Graph Protocol Indexing Agreement Cancellation",
        version: "0",
        chain_id: CHAIN_ID_ARBITRUM_ONE,
        salt: EIP712_DOMAIN_SALT,
    }
}

pub fn dips_collection_eip712_domain() -> Eip712Domain {
    eip712_domain! {
        name: "Graph Protocol Indexing Agreement Collection",
        version: "0",
        chain_id: CHAIN_ID_ARBITRUM_ONE,
        salt: EIP712_DOMAIN_SALT,
    }
}

sol! {
    // EIP712 encoded bytes
    #[derive(Debug, PartialEq)]
    struct SignedIndexingAgreementVoucher {
        IndexingAgreementVoucher voucher;
        bytes signature;
    }

    #[derive(Debug, PartialEq)]
    struct IndexingAgreementVoucher {
        // must be unique for each indexer/gateway pair
        bytes16 agreement_id;
        // should coincide with signer of this voucher
        address payer;
        // should coincide with indexer
        address recipient;
        // data service that will initiate payment collection
        address service;

        uint32 durationEpochs;

        uint256 maxInitialAmount;
        uint256 maxOngoingAmountPerEpoch;

        uint32 minEpochsPerCollection;
        uint32 maxEpochsPerCollection;

        // Deadline for the indexer to accept the agreement
        uint64 deadline;
        bytes metadata;
    }

    // the vouchers are generic to each data service, in the case of subgraphs this is an ABI-encoded SubgraphIndexingVoucherMetadata
    #[derive(Debug, PartialEq)]
    struct SubgraphIndexingVoucherMetadata {
        uint256 basePricePerEpoch; // wei GRT
        uint256 pricePerEntity; // wei GRT
        string subgraphDeploymentId; // e.g. "Qmbg1qF4YgHjiVfsVt6a13ddrVcRtWyJQfD4LA3CwHM29f" - TODO consider using bytes32
        string protocolNetwork; // e.g. "eip155:42161"
        string chainId; // indexed chain, e.g. "eip155:1"
    }

    #[derive(Debug, PartialEq)]
    struct SignedCancellationRequest {
        CancellationRequest request;
        bytes signature;
    }

    #[derive(Debug, PartialEq)]
    struct CancellationRequest {
        bytes16 agreement_id;
    }

    #[derive(Debug, PartialEq)]
    struct SignedCollectionRequest {
        CollectionRequest request;
        bytes signature;
    }

    #[derive(Debug, PartialEq)]
    struct CollectionRequest {
        bytes16 agreement_id;
        address allocation_id;
        uint64 entity_count;
    }

}

#[derive(Error, Debug)]
pub enum DipsError {
    // agreement cration
    #[error("signature is not valid, error: {0}")]
    InvalidSignature(String),
    #[error("payer {0} not authorised")]
    PayerNotAuthorised(Address),
    #[error("voucher payee {actual} does not match the expected address {expected}")]
    UnexpectedPayee { expected: Address, actual: Address },
    #[error("invalid subgraph id {0}")]
    InvalidSubgraphManifest(String),
    #[error("voucher for chain id {0}, subgraph manifest has network {1}")]
    SubgraphChainIdMistmatch(String, String),
    #[error("chainId {0} is not supported")]
    UnsupportedChainId(String),
    #[error("price per block is below configured price for chain {0}, minimum: {1}, offered: {2}")]
    PricePerBlockTooLow(String, u64, String),
    // cancellation
    #[error("cancelled_by is expected to match the signer")]
    UnexpectedSigner,
    #[error("signer {0} not authorised")]
    SignerNotAuthorised(Address),
    #[error("cancellation request has expired")]
    ExpiredRequest,
    // misc
    #[error("unknown error: {0}")]
    UnknownError(#[from] anyhow::Error),
    #[error("agreement not found")]
    AgreementNotFound,
    #[error("ABI decoding error: {0}")]
    AbiDecoding(String),
    #[error("agreement is cancelled")]
    AgreementCancelled,
    #[error("invalid voucher: {0}")]
    InvalidVoucher(String),
}

// TODO: send back messages
#[cfg(feature = "rpc")]
impl From<DipsError> for tonic::Status {
    fn from(_val: DipsError) -> Self {
        tonic::Status::internal("unknown errr")
    }
}

impl IndexingAgreementVoucher {
    pub fn sign<S: SignerSync>(
        &self,
        domain: &Eip712Domain,
        signer: S,
    ) -> anyhow::Result<SignedIndexingAgreementVoucher> {
        let voucher = SignedIndexingAgreementVoucher {
            voucher: self.clone(),
            signature: signer.sign_typed_data_sync(self, domain)?.as_bytes().into(),
        };

        Ok(voucher)
    }
}

impl SignedIndexingAgreementVoucher {
    // TODO: Validate all values
    pub fn validate(
        &self,
        signer_validator: &Arc<dyn signers::SignerValidator>,
        domain: &Eip712Domain,
        expected_payee: &Address,
        allowed_payers: impl AsRef<[Address]>,
    ) -> Result<(), DipsError> {
        let sig = Signature::from_str(&self.signature.to_string())
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        let payer = self.voucher.payer;
        let signer = sig
            .recover_address_from_prehash(&self.voucher.eip712_signing_hash(domain))
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        if allowed_payers.as_ref().is_empty()
            || !allowed_payers.as_ref().iter().any(|addr| addr.eq(&payer))
        {
            return Err(DipsError::PayerNotAuthorised(payer));
        }

        signer_validator
            .validate(&payer, &signer)
            .map_err(|_| DipsError::SignerNotAuthorised(signer))?;

        if !self.voucher.recipient.eq(expected_payee) {
            return Err(DipsError::UnexpectedPayee {
                expected: *expected_payee,
                actual: self.voucher.recipient,
            });
        }

        Ok(())
    }

    pub fn encode_vec(&self) -> Vec<u8> {
        self.abi_encode()
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

impl SignedCollectionRequest {
    pub fn encode_vec(&self) -> Vec<u8> {
        self.abi_encode()
    }
}

impl CollectionRequest {
    pub fn sign<S: SignerSync>(
        &self,
        domain: &Eip712Domain,
        signer: S,
    ) -> anyhow::Result<SignedCollectionRequest> {
        let voucher = SignedCollectionRequest {
            request: self.clone(),
            signature: signer.sign_typed_data_sync(self, domain)?.as_bytes().into(),
        };

        Ok(voucher)
    }
}

pub async fn validate_and_create_agreement(
    ctx: Arc<DipsServerContext>,
    domain: &Eip712Domain,
    expected_payee: &Address,
    allowed_payers: impl AsRef<[Address]>,
    voucher: Vec<u8>,
) -> Result<Uuid, DipsError> {
    let DipsServerContext {
        store,
        ipfs_fetcher,
        price_calculator,
        signer_validator,
    } = ctx.as_ref();
    let decoded_voucher = SignedIndexingAgreementVoucher::abi_decode(voucher.as_ref(), true)
        .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;
    let metadata = SubgraphIndexingVoucherMetadata::abi_decode(
        decoded_voucher.voucher.metadata.as_ref(),
        true,
    )
    .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

    decoded_voucher.validate(signer_validator, domain, expected_payee, allowed_payers)?;

    let manifest = ipfs_fetcher.fetch(&metadata.subgraphDeploymentId).await?;
    match manifest.network() {
        Some(chain_id) if chain_id == metadata.chainId => {}
        Some(chain_id) => {
            return Err(DipsError::SubgraphChainIdMistmatch(
                metadata.chainId,
                chain_id,
            ))
        }
        None => return Err(DipsError::UnsupportedChainId("".to_string())),
    }

    let chain_id = manifest
        .network()
        .ok_or_else(|| DipsError::UnsupportedChainId("".to_string()))?;

    let offered_price = metadata.pricePerEntity;
    match price_calculator.get_minimum_price(&chain_id) {
        Some(price) if offered_price.lt(&Uint::from(price)) => {
            return Err(DipsError::PricePerBlockTooLow(
                chain_id,
                price,
                offered_price.to_string(),
            ))
        }
        Some(_) => {}
        None => return Err(DipsError::UnsupportedChainId(chain_id)),
    }

    store
        .create_agreement(decoded_voucher.clone(), metadata)
        .await?;

    Ok(Uuid::from_bytes(
        decoded_voucher.voucher.agreement_id.into(),
    ))
}

pub async fn validate_and_cancel_agreement(
    store: Arc<dyn AgreementStore>,
    domain: &Eip712Domain,
    cancellation_request: Vec<u8>,
) -> Result<Uuid, DipsError> {
    let decoded_request =
        SignedCancellationRequest::abi_decode(cancellation_request.as_ref(), true)
            .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

    let result = store
        .get_by_id(Uuid::from_bytes(
            decoded_request.request.agreement_id.into(),
        ))
        .await?;
    let stored_agreement = result.ok_or(DipsError::AgreementNotFound)?;
    if stored_agreement.cancelled {
        return Err(DipsError::AgreementCancelled);
    }
    let expected_signer = stored_agreement.voucher.voucher.payer;
    let id = Uuid::from_bytes(decoded_request.request.agreement_id.into());
    decoded_request.validate(domain, &expected_signer)?;

    store.cancel_agreement(decoded_request).await?;

    Ok(id)
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use indexer_monitor::EscrowAccounts;
    use rand::{distr::Alphanumeric, Rng};
    use thegraph_core::alloy::{
        primitives::{Address, FixedBytes, U256},
        signers::local::PrivateKeySigner,
        sol_types::{Eip712Domain, SolValue},
    };
    use uuid::Uuid;

    pub use crate::store::{AgreementStore, InMemoryAgreementStore};
    use crate::{
        dips_agreement_eip712_domain, dips_cancellation_eip712_domain, server::DipsServerContext,
        CancellationRequest, DipsError, IndexingAgreementVoucher, SignedIndexingAgreementVoucher,
        SubgraphIndexingVoucherMetadata,
    };

    #[tokio::test]
    async fn test_validate_and_create_agreement() -> anyhow::Result<()> {
        let deployment_id = "Qmbg1qF4YgHjiVfsVt6a13ddrVcRtWyJQfD4LA3CwHM29f".to_string();
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "mainnet".to_string(),
            subgraphDeploymentId: deployment_id,
        };

        let voucher = IndexingAgreementVoucher {
            agreement_id: Uuid::now_v7().as_bytes().into(),
            payer: payer_addr,
            recipient: payee_addr,
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            deadline: 10000000,
            metadata: metadata.abi_encode().into(),
        };
        let domain = dips_agreement_eip712_domain();

        let voucher = voucher.sign(&domain, payer)?;
        let abi_voucher = voucher.abi_encode();
        let id = Uuid::from_bytes(voucher.voucher.agreement_id.into());

        let ctx = DipsServerContext::for_testing();
        let actual_id = super::validate_and_create_agreement(
            ctx.clone(),
            &domain,
            &payee_addr,
            vec![payer_addr],
            abi_voucher,
        )
        .await
        .unwrap();
        assert_eq!(actual_id, id);

        let stored_agreement = ctx.store.get_by_id(actual_id).await.unwrap().unwrap();

        assert_eq!(voucher, stored_agreement.voucher);
        assert!(!stored_agreement.cancelled);
        Ok(())
    }

    #[test]
    fn voucher_signature_verification() {
        let ctx = DipsServerContext::for_testing();
        let deployment_id = "Qmbg1qF4YgHjiVfsVt6a13ddrVcRtWyJQfD4LA3CwHM29f".to_string();
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            subgraphDeploymentId: deployment_id,
        };

        let voucher = IndexingAgreementVoucher {
            agreement_id: Uuid::now_v7().as_bytes().into(),
            payer: payer_addr,
            recipient: payee.address(),
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            deadline: 10000000,
            metadata: metadata.abi_encode().into(),
        };

        let domain = dips_agreement_eip712_domain();
        let signed = voucher.sign(&domain, payer).unwrap();
        assert_eq!(
            signed
                .validate(&ctx.signer_validator, &domain, &payee_addr, vec![])
                .unwrap_err()
                .to_string(),
            DipsError::PayerNotAuthorised(voucher.payer).to_string()
        );
        assert!(signed
            .validate(
                &ctx.signer_validator,
                &domain,
                &payee_addr,
                vec![payer_addr]
            )
            .is_ok());
    }

    #[tokio::test]
    async fn check_voucher_modified() {
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();
        let ctx = DipsServerContext::for_testing_mocked_accounts(EscrowAccounts::new(
            HashMap::default(),
            HashMap::from_iter(vec![(payer_addr, vec![payer_addr])]),
        ))
        .await;

        let deployment_id = "Qmbg1qF4YgHjiVfsVt6a13ddrVcRtWyJQfD4LA3CwHM29f".to_string();

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            subgraphDeploymentId: deployment_id,
        };

        let voucher = IndexingAgreementVoucher {
            agreement_id: Uuid::now_v7().as_bytes().into(),
            payer: payer_addr,
            recipient: payee_addr,
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            deadline: 10000000,
            metadata: metadata.abi_encode().into(),
        };
        let domain = dips_agreement_eip712_domain();

        let mut signed = voucher.sign(&domain, payer).unwrap();
        signed.voucher.service = Address::repeat_byte(9);

        assert!(matches!(
            signed
                .validate(
                    &ctx.signer_validator,
                    &domain,
                    &payee_addr,
                    vec![payer_addr]
                )
                .unwrap_err(),
            DipsError::SignerNotAuthorised(_)
        ));
    }

    #[test]
    fn cancel_voucher_validation() {
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();
        let other_signer = PrivateKeySigner::random();

        struct Case<'a> {
            name: &'a str,
            signer: PrivateKeySigner,
            error: Option<DipsError>,
        }

        let cases: Vec<Case> = vec![
            Case {
                name: "happy path payer",
                signer: payer.clone(),
                error: None,
            },
            Case {
                name: "invalid signer",
                signer: other_signer.clone(),
                error: Some(DipsError::SignerNotAuthorised(other_signer.address())),
            },
        ];

        for Case {
            name,
            signer,
            error,
        } in cases.into_iter()
        {
            let voucher = CancellationRequest {
                agreement_id: Uuid::now_v7().as_bytes().into(),
            };
            let domain = dips_cancellation_eip712_domain();

            let signed = voucher.sign(&domain, signer).unwrap();

            let res = signed.validate(&domain, &payer_addr);
            match error {
                Some(_err) => assert!(matches!(res.unwrap_err(), _err), "case: {}", name),
                None => assert!(res.is_ok(), "case: {}, err: {}", name, res.unwrap_err()),
            }
        }
    }
    struct VoucherContext {
        payee: PrivateKeySigner,
        payer: PrivateKeySigner,
        deployment_id: String,
    }

    impl VoucherContext {
        pub fn random() -> Self {
            Self {
                payee: PrivateKeySigner::random(),
                payer: PrivateKeySigner::random(),
                deployment_id: rand::rng()
                    .sample_iter(&Alphanumeric)
                    .take(32)
                    .map(char::from)
                    .collect(),
            }
        }
        pub fn domain(&self) -> Eip712Domain {
            dips_agreement_eip712_domain()
        }

        pub fn test_voucher_with_signer(
            &self,
            metadata: SubgraphIndexingVoucherMetadata,
            signer: PrivateKeySigner,
        ) -> SignedIndexingAgreementVoucher {
            let agreement_id = Uuid::now_v7();

            let domain = dips_agreement_eip712_domain();

            let voucher = IndexingAgreementVoucher {
                agreement_id: agreement_id.as_bytes().into(),
                payer: self.payer.address(),
                recipient: self.payee.address(),
                service: Address::ZERO,
                durationEpochs: 100,
                maxInitialAmount: U256::from(1000000_u64),
                maxOngoingAmountPerEpoch: U256::from(10000_u64),
                minEpochsPerCollection: 1,
                maxEpochsPerCollection: 10,
                deadline: (SystemTime::now() + Duration::from_secs(3600))
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                metadata: metadata.abi_encode().into(),
            };

            voucher.sign(&domain, signer).unwrap()
        }

        pub fn test_voucher(
            &self,
            metadata: SubgraphIndexingVoucherMetadata,
        ) -> SignedIndexingAgreementVoucher {
            self.test_voucher_with_signer(metadata, self.payer.clone())
        }
    }

    #[tokio::test]
    async fn test_create_and_cancel_agreement() -> anyhow::Result<()> {
        let ctx = DipsServerContext::for_testing();
        let voucher_ctx = VoucherContext::random();

        // Create metadata and voucher
        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "mainnet".to_string(),
            subgraphDeploymentId: voucher_ctx.deployment_id.clone(),
        };
        let signed_voucher = voucher_ctx.test_voucher(metadata);

        // Create agreement
        let agreement_id = super::validate_and_create_agreement(
            ctx.clone(),
            &voucher_ctx.domain(),
            &voucher_ctx.payee.address(),
            vec![voucher_ctx.payer.address()],
            signed_voucher.encode_vec(),
        )
        .await?;

        // Create and sign cancellation request
        let cancel_domain = dips_cancellation_eip712_domain();
        let cancel_request = CancellationRequest {
            agreement_id: agreement_id.as_bytes().into(),
        };
        let signed_cancel = cancel_request.sign(&cancel_domain, voucher_ctx.payer)?;

        // Cancel agreement
        let cancelled_id = super::validate_and_cancel_agreement(
            ctx.store.clone(),
            &cancel_domain,
            signed_cancel.encode_vec(),
        )
        .await?;

        assert_eq!(agreement_id, cancelled_id);

        // Verify agreement is cancelled
        let stored_agreement = ctx.store.get_by_id(agreement_id).await?.unwrap();
        assert!(stored_agreement.cancelled);

        Ok(())
    }

    #[tokio::test]
    async fn test_create_validations_errors() -> anyhow::Result<()> {
        let voucher_ctx = VoucherContext::random();
        let ctx = DipsServerContext::for_testing_mocked_accounts(EscrowAccounts::new(
            HashMap::default(),
            HashMap::from_iter(vec![(
                voucher_ctx.payer.address(),
                vec![voucher_ctx.payer.address()],
            )]),
        ))
        .await;

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "mainnet2".to_string(),
            subgraphDeploymentId: voucher_ctx.deployment_id.clone(),
        };

        let wrong_network_voucher = voucher_ctx.test_voucher(metadata);

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(10_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "mainnet".to_string(),
            subgraphDeploymentId: voucher_ctx.deployment_id.clone(),
        };

        let low_price_voucher = voucher_ctx.test_voucher(metadata);

        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "mainnet".to_string(),
            subgraphDeploymentId: voucher_ctx.deployment_id.clone(),
        };

        let signer = PrivateKeySigner::random();
        let valid_voucher_invalid_signer =
            voucher_ctx.test_voucher_with_signer(metadata.clone(), signer.clone());
        let valid_voucher = voucher_ctx.test_voucher(metadata);

        let expected_result: Vec<Result<[u8; 16], DipsError>> = vec![
            Err(DipsError::SubgraphChainIdMistmatch(
                "mainnet2".to_string(),
                "mainnet".to_string(),
            )),
            Err(DipsError::PricePerBlockTooLow(
                "mainnet".to_string(),
                100,
                "10".to_string(),
            )),
            Err(DipsError::SignerNotAuthorised(signer.address())),
            Ok(valid_voucher
                .voucher
                .agreement_id
                .as_slice()
                .try_into()
                .unwrap()),
        ];
        let cases = vec![
            wrong_network_voucher,
            low_price_voucher,
            valid_voucher_invalid_signer,
            valid_voucher,
        ];
        for (voucher, result) in cases.into_iter().zip(expected_result.into_iter()) {
            let out = super::validate_and_create_agreement(
                ctx.clone(),
                &voucher_ctx.domain(),
                &voucher_ctx.payee.address(),
                vec![voucher_ctx.payer.address()],
                voucher.encode_vec(),
            )
            .await;

            match (out, result) {
                (Ok(a), Ok(b)) => assert_eq!(a.into_bytes(), b),
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
                (a, b) => panic!("{:?} did not match {:?}", a, b),
            }
        }

        Ok(())
    }
}
