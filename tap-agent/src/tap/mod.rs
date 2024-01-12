// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use anyhow::anyhow;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;

mod escrow_adapter;
mod rav_storage_adapter;
mod receipt_checks_adapter;
mod receipt_storage_adapter;
mod sender_allocation_relationship;
pub mod sender_allocation_relationships_manager;

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
        .senders_to_signers
        .get(&sender)
        .ok_or(anyhow!("No signers found for sender {}.", sender))?
        .iter()
        .map(|s| s.to_string().trim_start_matches("0x").to_owned())
        .collect::<Vec<String>>();

    Ok(signers)
}
