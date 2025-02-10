// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::Display;

use thegraph_core::alloy::primitives::Address;

/// Wrapped Sender Address with two possible variants
///
/// This is used by children actors to define what kind of
/// SenderAllocation must be created to handle the correct
/// Rav and Receipt types
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum NetworkAddress {
    /// Legacy allocation
    Legacy(Address),
    /// New Subgraph DataService allocation
    Horizon(Address),
}

impl Display for NetworkAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkAddress::Legacy(address) | NetworkAddress::Horizon(address) => address.fmt(f),
        }
    }
}

impl NetworkAddress {
    /// Take the inner address for both allocation types
    pub fn address(&self) -> Address {
        match self {
            NetworkAddress::Legacy(address) | NetworkAddress::Horizon(address) => *address,
        }
    }
}

impl From<NetworkAddress> for String {
    fn from(value: NetworkAddress) -> Self {
        value.address().to_string()
    }
}
