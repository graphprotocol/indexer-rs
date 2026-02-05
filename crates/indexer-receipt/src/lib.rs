// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

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
pub enum TapReceipt {
    V2(tap_graph::v2::SignedReceipt),
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
            .map(|receipt| receipt.signed_receipt().get_v2_receipt().clone())
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
    pub fn as_v2(self) -> tap_graph::v2::SignedReceipt {
        match self {
            TapReceipt::V2(receipt) => receipt,
        }
    }

    pub fn get_v2_receipt(&self) -> &tap_graph::v2::SignedReceipt {
        match self {
            TapReceipt::V2(receipt) => receipt,
        }
    }

    pub fn collection_id(&self) -> FixedBytes<32> {
        self.get_v2_receipt().message.collection_id
    }

    pub fn signature(&self) -> Signature {
        self.get_v2_receipt().signature
    }

    pub fn nonce(&self) -> u64 {
        self.get_v2_receipt().message.nonce
    }

    pub fn recover_signer(
        &self,
        domain_separator: &Eip712Domain,
    ) -> Result<Address, tap_core::signed_message::Eip712Error> {
        self.get_v2_receipt().recover_signer(domain_separator)
    }
}

impl WithValueAndTimestamp for TapReceipt {
    fn value(&self) -> u128 {
        self.get_v2_receipt().value()
    }

    fn timestamp_ns(&self) -> u64 {
        self.get_v2_receipt().timestamp_ns()
    }
}

impl WithUniqueId for TapReceipt {
    type Output = SignatureBytes;

    fn unique_id(&self) -> Self::Output {
        self.get_v2_receipt().unique_id()
    }
}
