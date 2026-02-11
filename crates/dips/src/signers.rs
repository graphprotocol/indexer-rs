// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Signer authorization for DIPS agreements.
//!
//! When Dipper sends an RCA proposal, it's signed by a key that may differ from
//! the payer's address. Payers authorize signers via the PaymentsEscrow contract,
//! and this authorization data is indexed by the network subgraph.
//!
//! # How It Works
//!
//! [`EscrowSignerValidator`] wraps an `EscrowAccountsWatcher` that periodically
//! syncs escrow account data from the network subgraph. When validating an RCA:
//!
//! 1. Recover the signer address from the EIP-712 signature
//! 2. Look up authorized signers for the payer address
//! 3. Verify the recovered signer is in the authorized list
//!
//! # Security Considerations
//!
//! The network subgraph may lag behind chain state. This means:
//! - A newly authorized signer might be rejected briefly (UX issue, not security)
//! - A revoked signer might be accepted briefly (security concern)
//!
//! The **thawing period** on escrow withdrawals mitigates the second case.
//! Payers cannot withdraw funds instantly - they must wait through a thawing
//! period that exceeds the maximum expected subgraph lag. This gives indexers
//! time to collect owed fees before funds disappear.

use anyhow::anyhow;
#[cfg(test)]
use indexer_monitor::EscrowAccounts;
use indexer_monitor::EscrowAccountsWatcher;
use thegraph_core::alloy::primitives::Address;

pub trait SignerValidator: Sync + Send + std::fmt::Debug {
    fn validate(&self, payer: &Address, signer: &Address) -> Result<(), anyhow::Error>;
}

#[derive(Debug)]
pub struct EscrowSignerValidator {
    watcher: EscrowAccountsWatcher,
}

impl EscrowSignerValidator {
    pub fn new(watcher: EscrowAccountsWatcher) -> Self {
        Self { watcher }
    }

    #[cfg(test)]
    pub fn mock(accounts: EscrowAccounts) -> Self {
        let (_tx, rx) = tokio::sync::watch::channel(accounts);
        Self::new(rx)
    }
}

impl SignerValidator for EscrowSignerValidator {
    fn validate(&self, payer: &Address, signer: &Address) -> Result<(), anyhow::Error> {
        let signers = self.watcher.borrow().get_signers_for_sender(payer);

        if !signers.contains(signer) {
            return Err(anyhow!("Signer is not a valid signer for the sender"));
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct NoopSignerValidator;

impl SignerValidator for NoopSignerValidator {
    fn validate(&self, _payer: &Address, _signer: &Address) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use indexer_monitor::EscrowAccounts;
    use thegraph_core::alloy::primitives::Address;

    use crate::signers::SignerValidator;

    #[tokio::test]
    async fn test_escrow_validator() {
        let one = Address::ZERO;
        let two = Address::from_slice(&[1u8; 20]);
        let (_tx, watcher) = tokio::sync::watch::channel(EscrowAccounts::new(
            HashMap::default(),
            HashMap::from_iter(vec![(one, vec![two])]),
        ));

        let validator = super::EscrowSignerValidator::new(watcher);
        validator.validate(&one, &one).unwrap_err();
        validator.validate(&one, &two).unwrap();
    }
}
