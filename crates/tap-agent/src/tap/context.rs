// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{future::Future, marker::PhantomData};

use indexer_monitor::EscrowAccounts;
use indexer_receipt::TapReceipt;
use serde::Serialize;
use sqlx::PgPool;
use tap_aggregator::grpc::{
    v1::RavRequest as AggregatorRequestV1, v2::RavRequest as AggregatorRequestV2,
};
use tap_core::{
    receipt::{rav::Aggregate, WithValueAndTimestamp},
    signed_message::Eip712SignedMessage,
};
use thegraph_core::{
    alloy::{primitives::Address, sol_types::SolStruct},
    AllocationId as AllocationIdCore, CollectionId,
};
use tokio::sync::watch::Receiver;

pub mod checks;
mod error;
mod escrow;
mod rav;
mod receipt;

pub use error::AdapterError;
use tonic::{transport::Channel, Code, Status};

/// This trait represents a version of the network for TapAgentContext
///
/// It's used to define what Rav struct is used and how it handles
/// aggregations since each Rav version has its own aggregator client
///
/// The alternative would be using enum but this quickly scale into
/// multiple matches. This way we can keep the code separated and we
/// can easily add or remove network versions.
pub trait NetworkVersion: Send + Sync + 'static {
    /// The type used for allocation identifiers
    /// Legacy uses AllocationId (20 bytes), Horizon uses CollectionId (32 bytes) for collection IDs
    type AllocationId: Send + Sync + Clone + std::fmt::Debug + std::fmt::Display;

    /// Extract an Address for legacy compatibility (used for messaging)
    fn allocation_id_to_address(id: &Self::AllocationId) -> Address;

    /// Convert to the AllocationId enum for messaging
    fn to_allocation_id_enum(
        id: &Self::AllocationId,
    ) -> crate::agent::sender_accounts_manager::AllocationId;

    /// Sol struct returned from an aggregation
    ///
    /// Usually this is wrapped around a [Eip712SignedMessage].
    ///
    /// We provide all the trait bounds here to evict it spreading across other modules
    type Rav: SolStruct
        + Aggregate<TapReceipt>
        + Serialize
        + Send
        + Sync
        + WithValueAndTimestamp
        + Clone
        + std::fmt::Debug
        + PartialEq;

    /// gRPC client type used to process an aggregation request
    type AggregatorClient: Send + Sync;

    /// Takes the aggregator client, a list of receipts and the previous rav
    /// and performs an aggregation request
    fn aggregate(
        client: &mut Self::AggregatorClient,
        valid_receipts: Vec<TapReceipt>,
        previous_rav: Option<Eip712SignedMessage<Self::Rav>>,
    ) -> impl Future<Output = anyhow::Result<Eip712SignedMessage<Self::Rav>>> + Send;
}

/// 0-sized marker for legacy network
///
/// By using an enum with no variants, we prevent any instantiation
/// of the network. It also has zero size at runtime and is used only
/// as a compile-time marker
///
/// A simple `struct Legacy;` would be able to instantiate and pass as
/// value, while having size 1.
#[derive(Debug)]
pub enum Legacy {}
/// 0-sized marker for horizon network
///
/// By using an enum with no variants, we prevent any instantiation
/// of the network. It also has zero size at runtime and is used only
/// as a compile-time marker
///
/// A simple `struct Legacy;` would be able to instantiate and pass as
/// value, while having size 1.
#[derive(Debug)]
pub enum Horizon {}

impl NetworkVersion for Legacy {
    type AllocationId = AllocationIdCore;
    type Rav = tap_graph::ReceiptAggregateVoucher;
    type AggregatorClient =
        tap_aggregator::grpc::v1::tap_aggregator_client::TapAggregatorClient<Channel>;

    fn allocation_id_to_address(id: &Self::AllocationId) -> Address {
        **id // AllocationIdCore derefs to Address
    }

    fn to_allocation_id_enum(
        id: &Self::AllocationId,
    ) -> crate::agent::sender_accounts_manager::AllocationId {
        crate::agent::sender_accounts_manager::AllocationId::Legacy(*id)
    }

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

impl NetworkVersion for Horizon {
    type AllocationId = CollectionId;
    type Rav = tap_graph::v2::ReceiptAggregateVoucher;
    type AggregatorClient =
        tap_aggregator::grpc::v2::tap_aggregator_client::TapAggregatorClient<Channel>;

    fn allocation_id_to_address(id: &Self::AllocationId) -> Address {
        id.as_address()
    }

    fn to_allocation_id_enum(
        id: &Self::AllocationId,
    ) -> crate::agent::sender_accounts_manager::AllocationId {
        crate::agent::sender_accounts_manager::AllocationId::Horizon(*id)
    }

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

/// Context used by [tap_core::manager::Manager] that enables certain helper methods
///
/// This context is implemented for PostgresSQL
#[derive(Clone, bon::Builder)]
pub struct TapAgentContext<T> {
    pgpool: PgPool,
    /// For Legacy network: represents an allocation ID
    /// For Horizon network: represents a collection ID (stored in collection_id database column)
    #[cfg_attr(test, builder(default = crate::test::ALLOCATION_ID_0))]
    allocation_id: Address,
    #[cfg_attr(test, builder(default = test_assets::TAP_SENDER.1))]
    sender: Address,
    #[cfg_attr(test, builder(default = crate::test::INDEXER.1))]
    indexer_address: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    /// We use phantom data as a marker since it's
    /// only used to define what methods are available
    /// for each type of network
    #[builder(default = PhantomData)]
    _phantom: PhantomData<T>,
}
