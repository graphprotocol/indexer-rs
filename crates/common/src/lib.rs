// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

mod allocations;
pub mod attestation;
pub mod client;
mod dispute_manager;
pub mod escrow_accounts;
pub mod wallet;

pub mod monitors {
    pub use super::allocations::indexer_allocations;
    pub use super::attestation::attestation_signers;
    pub use super::dispute_manager::dispute_manager;
    pub use super::escrow_accounts::escrow_accounts;
}

pub use crate::client::{DeploymentDetails, SubgraphClient};
pub use escrow_accounts::{EscrowAccounts, EscrowAccountsError};
