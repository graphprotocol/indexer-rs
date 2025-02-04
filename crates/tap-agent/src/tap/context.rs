// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{future::Future, marker::PhantomData};

use indexer_monitor::EscrowAccounts;
use indexer_receipt::TapReceipt;
use serde::Serialize;
use sqlx::PgPool;
use tap_aggregator::grpc::v1::RavRequest as AggregatorRequestV1;
use tap_aggregator::grpc::v2::RavRequest as AggregatorRequestV2;
use tap_core::{
    receipt::{rav::Aggregate, WithValueAndTimestamp},
    signed_message::Eip712SignedMessage,
};
use thegraph_core::alloy::{primitives::Address, sol_types::SolStruct};
use tokio::sync::watch::Receiver;

pub mod checks;
mod error;
mod escrow;
mod rav;
mod receipt;

pub use error::AdapterError;
use tonic::{transport::Channel, Code, Status};

#[sealed::sealed]
pub trait ReceiptType: Send + Sync + 'static {
    //type Receipt;
    type Rav: SolStruct
        + Aggregate<TapReceipt>
        + Serialize
        + Send
        + Sync
        + WithValueAndTimestamp
        + Clone
        + std::fmt::Debug
        + PartialEq;

    type AggregatorClient: Send + Sync;

    fn aggregate(
        _client: &mut Self::AggregatorClient,
        _valid_receipts: Vec<TapReceipt>,
        _previous_rav: Option<Eip712SignedMessage<Self::Rav>>,
    ) -> impl Future<Output = anyhow::Result<Eip712SignedMessage<Self::Rav>>> + Send;
}

pub enum Legacy {}
pub enum Horizon {}

#[sealed::sealed]
impl ReceiptType for Legacy {
    //type Receipt = tap_graph::Receipt;
    type Rav = tap_graph::ReceiptAggregateVoucher;
    type AggregatorClient =
        tap_aggregator::grpc::v1::tap_aggregator_client::TapAggregatorClient<Channel>;

    async fn aggregate(
        client: &mut Self::AggregatorClient,
        valid_receipts: Vec<TapReceipt>,
        previous_rav: Option<Eip712SignedMessage<Self::Rav>>,
    ) -> anyhow::Result<Eip712SignedMessage<Self::Rav>> {
        let valid_receipts: Vec<_> = valid_receipts
            .into_iter()
            .map(|r| r.as_v1().ok_or(anyhow::anyhow!("Receipt is not legacy")))
            .collect::<Result<_, _>>()?;
        let rav_request = AggregatorRequestV1::new(valid_receipts, previous_rav);

        let response =
            client
                .aggregate_receipts(rav_request)
                .await
                .inspect_err(|status: &Status| {
                    if status.code() == Code::DeadlineExceeded {
                        tracing::warn!(
                            "Rav request is timing out, maybe request_timeout_secs is too \
                                low in your config file, try adding more secs to the value. \
                                If the problem persists after doing so please open an issue"
                        );
                    }
                })?;
        response.into_inner().signed_rav()
    }
}

#[sealed::sealed]
impl ReceiptType for Horizon {
    //type Receipt = tap_graph::v2::Receipt;
    type Rav = tap_graph::v2::ReceiptAggregateVoucher;
    type AggregatorClient =
        tap_aggregator::grpc::v2::tap_aggregator_client::TapAggregatorClient<Channel>;

    async fn aggregate(
        client: &mut Self::AggregatorClient,
        valid_receipts: Vec<TapReceipt>,
        previous_rav: Option<Eip712SignedMessage<Self::Rav>>,
    ) -> anyhow::Result<Eip712SignedMessage<Self::Rav>> {
        let valid_receipts: Vec<_> = valid_receipts
            .into_iter()
            .map(|r| r.as_v2().ok_or(anyhow::anyhow!("Receipt is not legacy")))
            .collect::<Result<_, _>>()?;
        let rav_request = AggregatorRequestV2::new(valid_receipts, previous_rav);

        let response =
            client
                .aggregate_receipts(rav_request)
                .await
                .inspect_err(|status: &Status| {
                    if status.code() == Code::DeadlineExceeded {
                        tracing::warn!(
                            "Rav request is timing out, maybe request_timeout_secs is too \
                                low in your config file, try adding more secs to the value. \
                                If the problem persists after doing so please open an issue"
                        );
                    }
                })?;
        response.into_inner().signed_rav()
    }
}

#[derive(Clone)]
pub struct TapAgentContext<T> {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    _phantom: PhantomData<T>,
}

impl<T: ReceiptType> TapAgentContext<T> {
    pub fn new(
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        escrow_accounts: Receiver<EscrowAccounts>,
    ) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            _phantom: PhantomData,
        }
    }
}
