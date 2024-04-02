// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use alloy_sol_types::{eip712_domain, Eip712Domain};
use ethers_signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use lazy_static::lazy_static;
use thegraph::types::Address;

lazy_static! {
    pub static ref ALLOCATION_ID_0: Address =
        Address::from_str("0xabababababababababababababababababababab").unwrap();
    pub static ref SENDER: (LocalWallet, Address) = wallet(0);
    pub static ref SIGNER: (LocalWallet, Address) = wallet(2);
    pub static ref TAP_EIP712_DOMAIN_SEPARATOR: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address:: from([0x11u8; 20]),
    };
}

/// Fixture to generate a wallet and address
pub fn wallet(index: u32) -> (LocalWallet, Address) {
    let wallet: LocalWallet = MnemonicBuilder::<English>::default()
        .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
        .index(index)
        .unwrap()
        .build()
        .unwrap();
    let address = wallet.address();
    (wallet, Address::from_slice(address.as_bytes()))
}
