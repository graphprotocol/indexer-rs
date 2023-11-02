// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use ethers::signers::{
    coins_bip39::English, LocalWallet, MnemonicBuilder, Signer, Wallet, WalletError,
};
use ethers_core::{k256::ecdsa::SigningKey, utils::hex};
use sha3::{Digest, Keccak256};

/// A normalized address in checksum format.
pub type Address = String;

/// Converts an address to checksum format and returns a typed instance.
pub fn to_address(s: impl AsRef<str>) -> Address {
    let address = s.as_ref().to_ascii_lowercase();
    let hash = &Keccak256::digest(address);
    hex::encode(hash)
}

/// Build Wallet from Private key or Mnemonic
pub fn build_wallet(value: &str) -> Result<Wallet<SigningKey>, WalletError> {
    value
        .parse::<LocalWallet>()
        .or(MnemonicBuilder::<English>::default().phrase(value).build())
}

/// Get wallet public address to String
pub fn wallet_address(wallet: &Wallet<SigningKey>) -> String {
    format!("{:?}", wallet.address())
}
