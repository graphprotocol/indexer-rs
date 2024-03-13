// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::hex::ToHex;
use anyhow::anyhow;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use thegraph::types::Address;

mod context;
mod escrow_adapter;
mod sender_account;
pub mod sender_accounts_manager;
mod sender_allocation;
mod unaggregated_receipts;

#[cfg(test)]
pub mod test_utils;

async fn signers_trimmed(
    escrow_accounts: &Eventual<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let signers = escrow_accounts
        .value()
        .await
        .map_err(|e| anyhow!("Error while getting escrow accounts: {:?}", e))?
        .get_signers_for_sender(&sender)?
        .iter()
        .map(|s| s.encode_hex::<String>())
        .collect::<Vec<String>>();

    Ok(signers)
}
