// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

pub use alloy;
pub use alloy_rlp;

use alloy::core::primitives::Address;
use alloy::rlp::{RlpDecodable, RlpEncodable};
use alloy::signers::Signature;
use alloy_sol_types::{sol, SolStruct};

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

impl SignedIndexingAgreementVoucher {
    // TODO: Validate all values, maybe return a useful error on failure.
    pub fn is_valid(
        &self,
        expected_payee: &Address,
        allowed_payers: impl AsRef<[Address]>,
    ) -> bool {
        let sig = match Signature::from_str(&self.signature.to_string()) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let payer = sig
            .recover_address_from_msg(self.voucher.eip712_hash_struct())
            .unwrap();

        if allowed_payers.as_ref().is_empty()
            || !allowed_payers.as_ref().iter().any(|addr| addr.eq(&payer))
        {
            return false;
        }

        if !self.voucher.payee.eq(expected_payee) {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod test {
    use alloy::primitives::{Address, FixedBytes, U256};
    use alloy::signers::local::PrivateKeySigner;
    use alloy::signers::SignerSync;
    use alloy::sol_types::SolStruct;

    use crate::{
        IndexingAgreementVoucher, SignedIndexingAgreementVoucher, SubgraphIndexingVoucherMetadata,
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

        let signed = SignedIndexingAgreementVoucher {
            voucher: voucher.clone(),
            signature: payer
                .sign_message_sync(voucher.eip712_hash_struct().as_slice())
                .unwrap()
                .as_bytes()
                .into(),
        };

        assert!(signed.is_valid(&payee_addr, vec![]));
        assert!(signed.is_valid(&payee_addr, vec![payer_addr]));
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

        let mut signed = SignedIndexingAgreementVoucher {
            voucher: voucher.clone(),
            signature: payer
                .sign_message_sync(voucher.eip712_hash_struct().as_slice())
                .unwrap()
                .as_bytes()
                .into(),
        };
        signed.voucher.service = Address::repeat_byte(9);

        assert!(signed.is_valid(&payee_addr, vec![payer_addr]));
    }
}
