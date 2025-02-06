// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

/// Struct used to represent summary of an allocation
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct UnaggregatedReceipts {
    /// Sum of Receipts value
    pub value: u128,
    /// The ID of the last receipt value added to the unaggregated fees value.
    /// This is used to make sure we don't process the same receipt twice. Relies on the fact that
    /// the receipts IDs are SERIAL in the database.
    pub last_id: u64,
    /// Counter of Receipts
    pub counter: u64,
}
