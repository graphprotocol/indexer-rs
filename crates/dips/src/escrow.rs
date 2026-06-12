// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Escrow fallback for agreement-proposal senders that are not on-chain agreement
//! managers: the role gate would reject them, but this lets one through when its
//! recovered signer resolves to a payer with enough escrow (a limited permissionless path).

use thegraph_core::alloy::primitives::{Address, U256};

/// Outcome of looking a signer up in the escrow snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowLookup {
    /// The snapshot has no signers at all (cold start or total outage), so the
    /// answer is "can't tell" rather than "unfunded".
    Unavailable,
    /// The signer resolved to a funded payer holding this balance (wei).
    Balance(U256),
    /// The snapshot is populated but the signer is not a funded payer.
    NotFound,
}

/// Read-side view of the escrow snapshot, keyed on the cryptographically
/// recovered signer (never a caller-written proposal field).
pub trait EscrowSource: std::fmt::Debug + Send + Sync {
    fn lookup(&self, signer: Address) -> EscrowLookup;
}

/// A fixed escrow snapshot. Used in tests and as a simple in-memory source;
/// production uses the shared TAP escrow watcher.
#[derive(Debug, Clone, Default)]
pub struct StaticEscrow(pub std::collections::HashMap<Address, U256>);

impl EscrowSource for StaticEscrow {
    fn lookup(&self, signer: Address) -> EscrowLookup {
        if self.0.is_empty() {
            return EscrowLookup::Unavailable;
        }
        match self.0.get(&signer) {
            Some(balance) => EscrowLookup::Balance(*balance),
            None => EscrowLookup::NotFound,
        }
    }
}

#[cfg(feature = "db")]
impl EscrowSource for indexer_monitor::EscrowAccountsWatcher {
    fn lookup(&self, signer: Address) -> EscrowLookup {
        let snapshot = self.borrow();
        if snapshot.signer_count() == 0 {
            return EscrowLookup::Unavailable;
        }
        match snapshot.get_balance_for_signer(&signer) {
            Ok(balance) => EscrowLookup::Balance(balance),
            Err(_) => EscrowLookup::NotFound,
        }
    }
}

#[cfg(test)]
mod test {
    use thegraph_core::alloy::primitives::{address, U256};

    use super::*;

    const SIGNER: Address = address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");

    #[test]
    fn empty_snapshot_is_unavailable() {
        assert_eq!(
            StaticEscrow::default().lookup(SIGNER),
            EscrowLookup::Unavailable
        );
    }

    #[test]
    fn known_signer_returns_balance() {
        let escrow = StaticEscrow([(SIGNER, U256::from(5))].into_iter().collect());
        assert_eq!(escrow.lookup(SIGNER), EscrowLookup::Balance(U256::from(5)));
    }

    #[test]
    fn populated_snapshot_without_signer_is_not_found() {
        let other = Address::repeat_byte(0x11);
        let escrow = StaticEscrow([(other, U256::from(5))].into_iter().collect());
        assert_eq!(escrow.lookup(SIGNER), EscrowLookup::NotFound);
    }
}
