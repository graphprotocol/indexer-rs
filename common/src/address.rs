// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::signers::local::{
    coins_bip39::English, LocalSignerError, MnemonicBuilder, PrivateKeySigner,
};
use bip39::Mnemonic;

/// Build Wallet from Private key or Mnemonic
pub fn build_wallet(value: &str) -> Result<PrivateKeySigner, LocalSignerError> {
    value
        .parse::<PrivateKeySigner>()
        .or(MnemonicBuilder::<English>::default().phrase(value).build())
}

// Format public key to a String
pub fn public_key(value: Mnemonic) -> Result<String, LocalSignerError> {
    let wallet = build_wallet(&value.to_string())?;
    let addr = format!("{:?}", wallet.address());
    Ok(addr)
}
