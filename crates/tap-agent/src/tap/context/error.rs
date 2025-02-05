// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use thegraph_core::alloy::primitives::Address;

/// AdapterError.
///
/// This is used to provide good error messages for indexers
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// Error in case it could not get escrow accounts
    #[error("Could not get escrow accounts from eventual")]
    EscrowEventualError {
        /// Error message
        error: String,
    },

    /// Error in case couldn't get the available escrow for a sender
    #[error("Could not get available escrow for sender")]
    AvailableEscrowError(#[from] indexer_monitor::EscrowAccountsError),

    /// Overflow error
    #[error("Sender {sender} escrow balance is too large to fit in u128, could not get available escrow.")]
    BalanceTooLarge {
        /// Sender address
        sender: Address,
    },

    /// Database error while storing rav
    #[error("Error in while storing Ravs: {error}")]
    RavStore {
        /// Error message
        error: String,
    },

    /// Database error while reading receipt
    #[error("Error in while reading Ravs: {error}")]
    RavRead {
        /// Error message
        error: String,
    },

    /// Database error while deleting receipt
    #[error("Error while deleting receipts: {error}")]
    ReceiptDelete {
        /// Error message
        error: String,
    },

    /// Database error while reading receipt
    #[error("Error while reading receipts: {error}")]
    ReceiptRead {
        /// Error message
        error: String,
    },

    /// Error validating signer for Ravs
    #[error("Error while validating rav signature: {error}")]
    ValidationError {
        /// Error message
        error: String,
    },
}
