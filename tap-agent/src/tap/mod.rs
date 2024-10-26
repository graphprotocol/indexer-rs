// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::hex::ToHexExt;
use alloy::primitives::Address;
use indexer_common::escrow_accounts::EscrowAccounts;
use tokio::sync::watch::Receiver;

pub mod context;
pub mod escrow_adapter;

#[cfg(test)]
pub mod test_utils;

pub async fn signers_trimmed(
    escrow_accounts: Receiver<EscrowAccounts> ,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let escrow_accounts = escrow_accounts.borrow();
    let signers = escrow_accounts
        .get_signers_for_sender(&sender)
        .iter()
        .map(|s| s.encode_hex())
        .collect::<Vec<String>>();
    Ok(signers)
}
