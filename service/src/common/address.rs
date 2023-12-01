// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use ethers::signers::{
    coins_bip39::English, LocalWallet, MnemonicBuilder, Signer, Wallet, WalletError,
};
use ethers_core::k256::ecdsa::SigningKey;

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
