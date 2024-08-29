// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct UnaggregatedReceipts {
    pub value: u128,
    /// The ID of the last receipt value added to the unaggregated fees value.
    /// This is used to make sure we don't process the same receipt twice. Relies on the fact that
    /// the receipts IDs are SERIAL in the database.
    pub last_id: u64,
}
