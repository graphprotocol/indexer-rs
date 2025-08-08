//! AllocationId wrapper enum for TAP Agent stream processing
//!
//! This module provides the unified AllocationId enum that wraps both Legacy (v1)
//! and Horizon (v2) allocation identifiers, enabling the stream processor to handle
//! both protocol versions during the migration period.

use std::fmt::Display;
use thegraph_core::{alloy::primitives::Address, AllocationId as AllocationIdCore, CollectionId};

/// Unified allocation identifier that supports both Legacy and Horizon protocols
///
/// This enum allows the TAP agent to process both v1 (Legacy) and v2 (Horizon)
/// allocation types in a type-safe manner during protocol migration.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AllocationId {
    /// Legacy v1 allocation: 20-byte allocation ID from original staking contracts
    Legacy(AllocationIdCore),
    /// Horizon v2 allocation: 32-byte collection ID from SubgraphService contracts
    Horizon(CollectionId),
}

impl AllocationId {
    /// Get a hex string representation suitable for database queries
    pub fn to_hex(&self) -> String {
        match self {
            AllocationId::Legacy(allocation_id) => allocation_id.to_string(),
            AllocationId::Horizon(collection_id) => collection_id.to_string(),
        }
    }

    /// Get the underlying Address for Legacy allocations only
    ///
    /// Returns None for Horizon allocations since they use 32-byte CollectionIds
    /// that don't directly correspond to allocation addresses.
    pub fn as_address(&self) -> Option<Address> {
        match self {
            AllocationId::Legacy(allocation_id) => Some(**allocation_id),
            AllocationId::Horizon(_) => None,
        }
    }

    /// Get an Address representation for both allocation types
    ///
    /// For Legacy: Returns the allocation address directly
    /// For Horizon: Returns the derived address from the CollectionId
    pub fn address(&self) -> Address {
        match self {
            AllocationId::Legacy(allocation_id) => **allocation_id,
            AllocationId::Horizon(collection_id) => collection_id.as_address(),
        }
    }
}

impl Display for AllocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocationId::Legacy(allocation_id) => write!(f, "{allocation_id}"),
            AllocationId::Horizon(collection_id) => write!(f, "{collection_id}"),
        }
    }
}
