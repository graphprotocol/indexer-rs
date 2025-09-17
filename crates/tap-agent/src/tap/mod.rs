// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # tap
//!
//! This module implements all traits and functions necessary for a context in a
//! [tap_core::manager::Manager] as well as receipt checks used to verify if receipts are valid.

pub use ::indexer_receipt::TapReceipt;
use indexer_monitor::EscrowAccounts;
use tap_core::receipt::{state::Checking, ReceiptWithState};
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};
use tokio::sync::watch::Receiver;

/// [TapReceipt] wrapped around a [Checking] state.
pub type CheckingReceipt = ReceiptWithState<Checking, TapReceipt>;

/// Context implmentation
pub mod context;

/// Helper function used to get a list of all signers in [String] for a given `sender`.
pub async fn signers_trimmed(
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let escrow_accounts = escrow_accounts_rx.borrow();

    tracing::info!(sender = %sender, "signers_trimmed called");

    let signers = escrow_accounts
        .get_signers_for_sender(&sender)
        .iter()
        .map(|s| s.encode_hex().trim_start_matches("0x").to_string())
        .collect::<Vec<String>>();

    Ok(signers)
}
