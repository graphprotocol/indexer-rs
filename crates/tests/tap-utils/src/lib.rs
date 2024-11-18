// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{
    dyn_abi::Eip712Domain,
    signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
};
use lazy_static::lazy_static;
use tap_core::{
    receipt::{Receipt, SignedReceipt},
    signed_message::EIP712SignedMessage,
    tap_eip712_domain,
};
use thegraph_core::Address;

lazy_static! {

    /// Fixture to generate a wallet and address.
    /// Address: 0x9858EfFD232B4033E47d90003D41EC34EcaEda94
    pub static ref TAP_SENDER: (PrivateKeySigner, Address) = {
        let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, address)
    };

    /// Fixture to generate a wallet and address.
    /// Address: 0x533661F0fb14d2E8B26223C86a610Dd7D2260892
    pub static ref TAP_SIGNER: (PrivateKeySigner, Address) = {
        let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
            .phrase("rude pipe parade travel organ vendor card festival magnet novel forget refuse keep draft tool")
            .build()
            .unwrap();
        let address = wallet.address();

        (wallet, address)
    };

    pub static ref TAP_EIP712_DOMAIN: Eip712Domain = tap_eip712_domain(
        1,
        Address::from([0x11u8; 20])
    );
}

/// Function to generate a signed receipt using the TAP_SIGNER wallet.
pub async fn create_signed_receipt(
    allocation_id: Address,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
) -> SignedReceipt {
    let (wallet, _) = &*self::TAP_SIGNER;

    EIP712SignedMessage::new(
        &self::TAP_EIP712_DOMAIN,
        Receipt {
            allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        wallet,
    )
    .unwrap()
}
