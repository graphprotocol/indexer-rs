// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::hex::ToHexExt;
use indexer_common::escrow_accounts::EscrowAccounts;
use thegraph_core::Address;
use tokio::sync::watch::Receiver;

pub mod context;
pub mod escrow_adapter;

#[cfg(test)]
pub mod test_utils;

pub async fn signers_trimmed(
    escrow_accounts: Receiver<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    //change removed error .map_err(|e| anyhow!("Error while getting escrow accounts: {:?}", e))?
    let signers = escrow_accounts
        .borrow()
        .clone()
        .get_signers_for_sender(&sender)
        .iter()
        .map(|s| s.encode_hex())
        .collect::<Vec<String>>();

    Ok(signers)
}
