// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

pub use alloy;
pub use alloy_rlp;

use alloy::core::primitives::Address;
use alloy::rlp::{RlpDecodable, RlpEncodable};
use alloy::signers::{Signature, SignerSync};
use alloy_sol_types::{sol, Eip712Domain, SolStruct};
use thiserror::Error;

sol! {
    // EIP712 encoded bytes, ABI - ethers
    #[derive(Debug, RlpEncodable, RlpDecodable, PartialEq)]
    struct SignedIndexingAgreementVoucher {
        IndexingAgreementVoucher voucher;
        bytes signature;
    }

    #[derive(Debug, RlpEncodable, RlpDecodable, PartialEq)]
    struct IndexingAgreementVoucher {
      // should coincide with signer
        address payer;
        // should coincide with indexer
        address payee;
         // data service that will initiate payment collection
        address service;
        // initial indexing amount max
        uint256 maxInitialAmount;
        uint256 maxOngoingAmountPerEpoch;
        // time to accept the agreement, intended to be on the order
        // of hours or mins
        uint64 deadline;
        uint32 maxEpochsPerCollection;
        uint32 minEpochsPerCollection;
        // after which the agreement is complete
        uint32 durationEpochs;
        bytes metadata;
    }

    // the vouchers are generic to each data service, in the case of subgraphs this is an ABI-encoded SubgraphIndexingVoucherMetadata
    #[derive(Debug, RlpEncodable, RlpDecodable, PartialEq)]
    struct SubgraphIndexingVoucherMetadata {
        uint256 pricePerBlock; // wei GRT
        bytes32 protocolNetwork; // eip199:1 format
        // differentiate based on indexed chain
        bytes32 chainId; // eip199:1 format
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum AgreementVoucherValidationError {
    #[error("signature is not valid, error: {0}")]
    InvalidSignature(String),
    #[error("payer {0} not authorised")]
    PayerNotAuthorised(Address),
    #[error("voucher payee {actual} does not match the expected address {expected}")]
    UnexpectedPayee { expected: Address, actual: Address },
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
    ) -> Result<(), AgreementVoucherValidationError> {
        let sig = Signature::from_str(&self.signature.to_string())
            .map_err(|err| AgreementVoucherValidationError::InvalidSignature(err.to_string()))?;

        let payer = sig
            .recover_address_from_prehash(&self.voucher.eip712_signing_hash(domain))
            .map_err(|err| AgreementVoucherValidationError::InvalidSignature(err.to_string()))?;

        if allowed_payers.as_ref().is_empty()
            || !allowed_payers.as_ref().iter().any(|addr| addr.eq(&payer))
        {
            return Err(AgreementVoucherValidationError::PayerNotAuthorised(payer));
        }

        if !self.voucher.payee.eq(expected_payee) {
            return Err(AgreementVoucherValidationError::UnexpectedPayee {
                expected: *expected_payee,
                actual: self.voucher.payee,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use alloy::primitives::{Address, FixedBytes, U256};
    use alloy::signers::local::PrivateKeySigner;
    use alloy::sol_types::SolStruct;
    use thegraph_core::attestation::eip712_domain;

    use crate::{
        AgreementVoucherValidationError, IndexingAgreementVoucher, SubgraphIndexingVoucherMetadata,
    };

    #[test]
    fn voucher_signature_verification() {
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();

        let metadata = SubgraphIndexingVoucherMetadata {
            pricePerBlock: U256::from(10000_u64),
            protocolNetwork: FixedBytes::left_padding_from("arbitrum-one".as_bytes()),
            chainId: FixedBytes::left_padding_from("mainnet".as_bytes()),
        };

        let voucher = IndexingAgreementVoucher {
            payer: payer_addr,
            payee: payee.address(),
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            deadline: 1000,
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            metadata: metadata.eip712_hash_struct().to_owned().into(),
        };

        let domain = eip712_domain(0, Address::ZERO);
        let signed = voucher.sign(&domain, payer).unwrap();
        assert_eq!(
            signed.validate(&domain, &payee_addr, vec![]).unwrap_err(),
            AgreementVoucherValidationError::PayerNotAuthorised(voucher.payer)
        );
        assert!(signed
            .validate(&domain, &payee_addr, vec![payer_addr])
            .is_ok());
    }

    #[test]
    fn check_voucher_modified() {
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();

        let metadata = SubgraphIndexingVoucherMetadata {
            pricePerBlock: U256::from(10000_u64),
            protocolNetwork: FixedBytes::left_padding_from("arbitrum-one".as_bytes()),
            chainId: FixedBytes::left_padding_from("mainnet".as_bytes()),
        };

        let voucher = IndexingAgreementVoucher {
            payer: payer_addr,
            payee: payee_addr,
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            deadline: 1000,
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            metadata: metadata.eip712_hash_struct().to_owned().into(),
        };
        let domain = eip712_domain(0, Address::ZERO);

        let mut signed = voucher.sign(&domain, payer).unwrap();
        signed.voucher.service = Address::repeat_byte(9);

        assert!(matches!(
            signed
                .validate(&domain, &payee_addr, vec![payer_addr])
                .unwrap_err(),
            AgreementVoucherValidationError::PayerNotAuthorised(_)
        ));
    }
}
