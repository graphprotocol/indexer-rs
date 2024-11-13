// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use thegraph_core::Attestation;

pub mod dispute_manager;
pub mod signer;
pub mod signers;

pub trait AttestableResponse<T> {
    fn as_str(&self) -> &str;
    fn finalize(self, attestation: AttestationOutput) -> T;
    fn is_attestable(&self) -> bool;
}

pub enum AttestationOutput {
    Attestation(Option<Attestation>),
    Attestable,
}
