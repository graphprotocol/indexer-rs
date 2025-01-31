// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc};

use thegraph_core::alloy::{
    core::primitives::Address,
    primitives::{b256, ChainId, PrimitiveSignature as Signature, B256},
    signers::SignerSync,
    sol,
    sol_types::{eip712_domain, Eip712Domain, SolStruct, SolValue},
};

pub mod proto;
pub mod server;
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
        domain: &Eip712Domain,
        expected_payee: &Address,
        allowed_payers: impl AsRef<[Address]>,
    ) -> Result<(), DipsError> {
        let sig = Signature::from_str(&self.signature.to_string())
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        let payer = sig
            .recover_address_from_prehash(&self.voucher.eip712_signing_hash(domain))
            .map_err(|err| DipsError::InvalidSignature(err.to_string()))?;

        if allowed_payers.as_ref().is_empty()
            || !allowed_payers.as_ref().iter().any(|addr| addr.eq(&payer))
        {
            return Err(DipsError::PayerNotAuthorised(payer));
        }

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
    store: Arc<dyn AgreementStore>,
    domain: &Eip712Domain,
    expected_payee: &Address,
    allowed_payers: impl AsRef<[Address]>,
    voucher: Vec<u8>,
) -> Result<Uuid, DipsError> {
    let decoded_voucher = SignedIndexingAgreementVoucher::abi_decode(voucher.as_ref(), true)
        .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;
    let metadata = SubgraphIndexingVoucherMetadata::abi_decode(
        decoded_voucher.voucher.metadata.as_ref(),
        true,
    )
    .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;

    decoded_voucher.validate(domain, expected_payee, allowed_payers)?;

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
    let (agreement, cancelled) = result.ok_or(DipsError::AgreementNotFound)?;
    if cancelled {
        return Err(DipsError::AgreementCancelled);
    }
    let expected_signer = agreement.voucher.payer;
    let id = Uuid::from_bytes(decoded_request.request.agreement_id.into());
    decoded_request.validate(domain, &expected_signer)?;

    store.cancel_agreement(decoded_request).await?;

    Ok(id)
}

#[cfg(test)]
mod test {
    use std::{
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use thegraph_core::alloy::{
        primitives::{Address, FixedBytes, U256},
        signers::local::PrivateKeySigner,
        sol_types::SolValue,
    };
    use uuid::Uuid;

    pub use crate::store::{AgreementStore, InMemoryAgreementStore};
    use crate::{
        dips_agreement_eip712_domain, dips_cancellation_eip712_domain, CancellationRequest,
        DipsError, IndexingAgreementVoucher, SubgraphIndexingVoucherMetadata,
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

        let voucher = voucher.sign(&domain, payer)?;
        let abi_voucher = voucher.abi_encode();
        let id = Uuid::from_bytes(voucher.voucher.agreement_id.into());

        let store = Arc::new(InMemoryAgreementStore::default());

        let actual_id = super::validate_and_create_agreement(
            store.clone(),
            &domain,
            &payee_addr,
            vec![payer_addr],
            abi_voucher,
        )
        .await
        .unwrap();
        assert_eq!(actual_id, id);

        let actual = store.get_by_id(actual_id).await.unwrap();

        let (actual_voucher, actual_cancelled) = actual.unwrap();
        assert_eq!(voucher, actual_voucher);
        assert!(!actual_cancelled);
        Ok(())
    }

    #[test]
    fn voucher_signature_verification() {
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
                .validate(&domain, &payee_addr, vec![])
                .unwrap_err()
                .to_string(),
            DipsError::PayerNotAuthorised(voucher.payer).to_string()
        );
        assert!(signed
            .validate(&domain, &payee_addr, vec![payer_addr])
            .is_ok());
    }

    #[test]
    fn check_voucher_modified() {
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
                .validate(&domain, &payee_addr, vec![payer_addr])
                .unwrap_err(),
            DipsError::PayerNotAuthorised(_)
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
    #[tokio::test]
    async fn test_create_and_cancel_agreement() -> anyhow::Result<()> {
        let deployment_id = "Qmbg1qF4YgHjiVfsVt6a13ddrVcRtWyJQfD4LA3CwHM29f".to_string();
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();
        let store = Arc::new(InMemoryAgreementStore::default());

        // Create metadata and voucher
        let metadata = SubgraphIndexingVoucherMetadata {
            basePricePerEpoch: U256::from(10000_u64),
            pricePerEntity: U256::from(100_u64),
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            subgraphDeploymentId: deployment_id,
        };

        let agreement_id = Uuid::now_v7();
        let voucher = IndexingAgreementVoucher {
            agreement_id: agreement_id.as_bytes().into(),
            payer: payer_addr,
            recipient: payee_addr,
            service: Address::ZERO,
            durationEpochs: 100,
            maxInitialAmount: U256::from(1000000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            minEpochsPerCollection: 1,
            maxEpochsPerCollection: 10,
            deadline: (SystemTime::now() + Duration::from_secs(3600))
                .duration_since(UNIX_EPOCH)?
                .as_secs(),
            metadata: metadata.abi_encode().into(),
        };

        let domain = dips_agreement_eip712_domain();
        let signed_voucher = voucher.sign(&domain, payer.clone())?;

        // Create agreement
        let agreement_id = super::validate_and_create_agreement(
            store.clone(),
            &domain,
            &payee_addr,
            vec![payer_addr],
            signed_voucher.encode_vec(),
        )
        .await?;

        // Create and sign cancellation request
        let cancel_domain = dips_cancellation_eip712_domain();
        let cancel_request = CancellationRequest {
            agreement_id: agreement_id.as_bytes().into(),
        };
        let signed_cancel = cancel_request.sign(&cancel_domain, payer)?;

        // Cancel agreement
        let cancelled_id = super::validate_and_cancel_agreement(
            store.clone(),
            &cancel_domain,
            signed_cancel.encode_vec(),
        )
        .await?;

        assert_eq!(agreement_id, cancelled_id);

        // Verify agreement is cancelled
        let result = store.get_by_id(agreement_id).await?;
        let (_, cancelled) = result.ok_or(DipsError::AgreementNotFound)?;
        assert!(cancelled);

        Ok(())
    }
}
