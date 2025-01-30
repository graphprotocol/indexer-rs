// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use tap_core::{
    receipt::{WithUniqueId, WithValueAndTimestamp},
    signed_message::SignatureBytes,
};
use thegraph_core::alloy::{dyn_abi::Eip712Domain, primitives::Address, signers::Signature};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapReceipt {
    V1(tap_graph::SignedReceipt),
    V2(tap_graph::v2::SignedReceipt),
}

impl TapReceipt {
    pub fn allocation_id(&self) -> Address {
        match self {
            TapReceipt::V1(receipt) => receipt.message.allocation_id,
            TapReceipt::V2(receipt) => receipt.message.allocation_id,
        }
    }

    pub fn signature(&self) -> Signature {
        match self {
            TapReceipt::V1(receipt) => receipt.signature,
            TapReceipt::V2(receipt) => receipt.signature,
        }
    }

    pub fn nonce(&self) -> u64 {
        match self {
            TapReceipt::V1(receipt) => receipt.message.nonce,
            TapReceipt::V2(receipt) => receipt.message.nonce,
        }
    }

    pub fn recover_signer(
        &self,
        domain_separator: &Eip712Domain,
    ) -> Result<Address, tap_core::signed_message::Eip712Error> {
        match self {
            TapReceipt::V1(receipt) => receipt.recover_signer(domain_separator),
            TapReceipt::V2(receipt) => receipt.recover_signer(domain_separator),
        }
    }
}

impl WithValueAndTimestamp for TapReceipt {
    fn value(&self) -> u128 {
        match self {
            TapReceipt::V1(receipt) => receipt.value(),
            TapReceipt::V2(receipt) => receipt.value(),
        }
    }

    fn timestamp_ns(&self) -> u64 {
        match self {
            TapReceipt::V1(receipt) => receipt.timestamp_ns(),
            TapReceipt::V2(receipt) => receipt.timestamp_ns(),
        }
    }
}

impl WithUniqueId for TapReceipt {
    type Output = SignatureBytes;

    fn unique_id(&self) -> Self::Output {
        match self {
            TapReceipt::V1(receipt) => receipt.unique_id(),
            TapReceipt::V2(receipt) => receipt.unique_id(),
        }
    }
}
