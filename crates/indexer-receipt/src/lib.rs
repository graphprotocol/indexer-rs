// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use tap_core::{
    receipt::{
        rav::{Aggregate, AggregationError},
        WithUniqueId, WithValueAndTimestamp,
    },
    signed_message::SignatureBytes,
};
use thegraph_core::alloy::{
    dyn_abi::Eip712Domain,
    primitives::{Address, FixedBytes},
    signers::Signature,
};

// SHARED: V1 + V2 common code
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapReceipt {
    V1(tap_graph::SignedReceipt),
    V2(tap_graph::v2::SignedReceipt),
}

// V1_LEGACY: Out of scope for Horizon security audit
impl Aggregate<TapReceipt> for tap_graph::ReceiptAggregateVoucher {
    fn aggregate_receipts(
        receipts: &[tap_core::receipt::ReceiptWithState<
            tap_core::receipt::state::Checked,
            TapReceipt,
        >],
        previous_rav: Option<tap_core::signed_message::Eip712SignedMessage<Self>>,
    ) -> Result<Self, tap_core::receipt::rav::AggregationError> {
        if receipts.is_empty() {
            return Err(AggregationError::NoValidReceiptsForRavRequest);
        }
        let receipts: Vec<_> = receipts
            .iter()
            .map(|receipt| {
                receipt
                    .signed_receipt()
                    .get_v1_receipt()
                    .cloned()
                    .ok_or(anyhow!("Receipt is not v1"))
            })
            .collect::<Result<_, _>>()
            .map_err(AggregationError::Other)?;
        let allocation_id = receipts[0].message.allocation_id;
        tap_graph::ReceiptAggregateVoucher::aggregate_receipts(
            allocation_id,
            receipts.as_slice(),
            previous_rav,
        )
    }
}

impl Aggregate<TapReceipt> for tap_graph::v2::ReceiptAggregateVoucher {
    fn aggregate_receipts(
        receipts: &[tap_core::receipt::ReceiptWithState<
            tap_core::receipt::state::Checked,
            TapReceipt,
        >],
        previous_rav: Option<tap_core::signed_message::Eip712SignedMessage<Self>>,
    ) -> Result<Self, tap_core::receipt::rav::AggregationError> {
        if receipts.is_empty() {
            return Err(AggregationError::NoValidReceiptsForRavRequest);
        }
        let receipts: Vec<_> = receipts
            .iter()
            .map(|receipt| {
                receipt
                    .signed_receipt()
                    .get_v2_receipt()
                    .cloned()
                    .ok_or(anyhow!("Receipt is not v2"))
            })
            .collect::<Result<_, _>>()
            .map_err(AggregationError::Other)?;
        let collection_id = receipts[0].message.collection_id;
        let payer = receipts[0].message.payer;
        let data_service = receipts[0].message.data_service;
        let service_provider = receipts[0].message.service_provider;

        tap_graph::v2::ReceiptAggregateVoucher::aggregate_receipts(
            collection_id,
            payer,
            data_service,
            service_provider,
            receipts.as_slice(),
            previous_rav,
        )
    }
}

impl TapReceipt {
    pub fn as_v1(self) -> Option<tap_graph::SignedReceipt> {
        match self {
            TapReceipt::V1(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub fn as_v2(self) -> Option<tap_graph::v2::SignedReceipt> {
        match self {
            TapReceipt::V2(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub fn get_v1_receipt(&self) -> Option<&tap_graph::SignedReceipt> {
        match self {
            TapReceipt::V1(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub fn get_v2_receipt(&self) -> Option<&tap_graph::v2::SignedReceipt> {
        match self {
            TapReceipt::V2(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub fn allocation_id(&self) -> Option<Address> {
        match self {
            TapReceipt::V1(receipt) => Some(receipt.message.allocation_id),
            _ => None,
        }
    }

    pub fn collection_id(&self) -> Option<FixedBytes<32>> {
        match self {
            TapReceipt::V2(receipt) => Some(receipt.message.collection_id),
            _ => None,
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
