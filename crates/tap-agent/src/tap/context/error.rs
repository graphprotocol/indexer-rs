// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::primitives::Address;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("Could not get escrow accounts from eventual")]
    EscrowEventualError { error: String },

    #[error("Could not get available escrow for sender")]
    AvailableEscrowError(#[from] indexer_common::escrow_accounts::EscrowAccountsError),

    #[error("Sender {sender} escrow balance is too large to fit in u128, could not get available escrow.")]
    BalanceTooLarge { sender: Address },

    #[error("Sender {sender} does not have enough escrow to subtract {fees} from {balance}.")]
    NotEnoughEscrow {
        sender: Address,
        fees: u128,
        balance: u128,
    },

    #[error("Error in while storing Ravs: {error}")]
    RavStore { error: String },

    #[error("Error in while reading Ravs: {error}")]
    RavRead { error: String },

    #[error("Error while deleting receipts: {error}")]
    ReceiptDelete { error: String },

    #[error("Error while reading receipts: {error}")]
    ReceiptRead { error: String },

    #[error("Error while validating receipts: {error}")]
    ValidationError { error: String },
}
