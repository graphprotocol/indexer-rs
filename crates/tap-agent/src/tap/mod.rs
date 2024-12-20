// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_monitor::EscrowAccounts;
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};
use tokio::sync::watch::Receiver;

pub mod context;

pub async fn signers_trimmed(
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let escrow_accounts = escrow_accounts_rx.borrow();
    let signers = escrow_accounts
        .get_signers_for_sender(&sender)
        .iter()
        .map(|s| s.encode_hex())
        .collect::<Vec<String>>();
    Ok(signers)
}
