// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::hex::ToHexExt;
use alloy::primitives::Address;
use anyhow::anyhow;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;

pub mod context;
pub mod escrow_adapter;

#[cfg(test)]
pub mod test_utils;

pub async fn signers_trimmed(
    escrow_accounts: &Eventual<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let signers = escrow_accounts
        .value()
        .await
        .map_err(|e| anyhow!("Error while getting escrow accounts: {:?}", e))?
        .get_signers_for_sender(&sender)
        .iter()
        .map(|s| s.encode_hex())
        .collect::<Vec<String>>();

    Ok(signers)
}
