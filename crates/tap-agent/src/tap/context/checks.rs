// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # Receipt Checks
//!
//! Some additional receipt checks are done before an aggregation request.
//! This allows us to have two layers of checks that allows some relieve in the performance
//! critical part of the system in indexer-service

mod allocation_id;
mod signature;

pub use allocation_id::AllocationId;
pub use signature::Signature;
