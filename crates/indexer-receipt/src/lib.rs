// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::ops::Deref;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapReceipt(pub tap_graph::v2::SignedReceipt);

impl Deref for TapReceipt {
    type Target = tap_graph::v2::SignedReceipt;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<tap_graph::v2::SignedReceipt> for TapReceipt {
    fn as_ref(&self) -> &tap_graph::v2::SignedReceipt {
        &self.0
    }
}

impl From<tap_graph::v2::SignedReceipt> for TapReceipt {
    fn from(receipt: tap_graph::v2::SignedReceipt) -> Self {
        Self(receipt)
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
            .map(|receipt| receipt.signed_receipt().0.clone())
            .collect();
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
    pub fn collection_id(&self) -> FixedBytes<32> {
        self.0.message.collection_id
    }

    pub fn signature(&self) -> Signature {
        self.0.signature
    }

    pub fn nonce(&self) -> u64 {
        self.0.message.nonce
    }

    pub fn recover_signer(
        &self,
        domain_separator: &Eip712Domain,
    ) -> Result<Address, tap_core::signed_message::Eip712Error> {
        self.0.recover_signer(domain_separator)
    }
}

impl WithValueAndTimestamp for TapReceipt {
    fn value(&self) -> u128 {
        self.0.value()
    }

    fn timestamp_ns(&self) -> u64 {
        self.0.timestamp_ns()
    }
}

impl WithUniqueId for TapReceipt {
    type Output = SignatureBytes;

    fn unique_id(&self) -> Self::Output {
        self.0.unique_id()
    }
}
