// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    future::Future,
    marker::PhantomData,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure};
use bigdecimal::{num_bigint::BigInt, ToPrimitive};
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use itertools::{Either, Itertools};
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{
    manager::adapters::{RavRead, RavStore, ReceiptDelete, ReceiptRead},
    rav_request::RavRequest,
    receipt::{
        checks::{Check, CheckList},
        rav::AggregationError,
        state::Failed,
        Context, ReceiptWithState, WithValueAndTimestamp,
    },
    signed_message::Eip712SignedMessage,
};
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address, sol_types::Eip712Domain};
use thiserror::Error;
use tokio::sync::watch::Receiver;

use super::sender_account::SenderAccountConfig;
use crate::{
    agent::{
        sender_account::{RavInformation, ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::NewReceiptNotification,
        unaggregated_receipts::UnaggregatedReceipts,
    },
    tap::{
        context::{
            checks::{AllocationId, Signature},
            Horizon, Legacy, NetworkVersion, TapAgentContext,
        },
        signers_trimmed, TapReceipt,
    },
};

static CLOSED_SENDER_ALLOCATIONS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_closed_sender_allocation_total",
        "Count of sender-allocation managers closed since the start of the program",
        &["sender"]
    )
    .unwrap()
});
static RAVS_CREATED: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_ravs_created_total",
        "RAVs updated or created per sender allocation since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap()
});
static RAVS_FAILED: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_ravs_failed_total",
        "RAV requests failed since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap()
});
static RAV_RESPONSE_TIME: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "tap_rav_response_time_seconds",
        "RAV response time per sender",
        &["sender"]
    )
    .unwrap()
});

/// Possible Rav Errors returned in case of a failure in Rav Request
///
/// This is used to give better error messages to users so they have a better understanding
#[derive(Error, Debug)]
pub enum RavError {
    /// Database Errors
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    /// Tap Core lib errors
    #[error(transparent)]
    TapCore(#[from] tap_core::Error),

    /// Errors while aggregating
    #[error(transparent)]
    AggregationError(#[from] AggregationError),

    /// Errors with gRPC client
    #[error(transparent)]
    Grpc(#[from] tonic::Status),

    /// All receipts are invalid
    #[error("All receipts are invalid")]
    AllReceiptsInvalid,

    /// Other kind of error
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type TapManager<T> = tap_core::manager::Manager<TapAgentContext<T>, TapReceipt>;

/// Manages unaggregated fees and the TAP lifecyle for a specific (allocation, sender) pair.
///
/// We use PhantomData to be able to add bounds to T while implementing the Actor trait
///
/// T is used in SenderAllocationState<T> and SenderAllocationArgs<T> to store the
/// correct Rav type and the correct aggregator client
pub struct SenderAllocation<T>(PhantomData<T>);
impl<T: NetworkVersion> Default for SenderAllocation<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

/// State for [SenderAllocation] actor
pub struct SenderAllocationState<T: NetworkVersion> {
    /// Sum of all receipt fees for the current allocation
    unaggregated_fees: UnaggregatedReceipts,
    /// Sum of all invalid receipts for the current allocation
    invalid_receipts_fees: UnaggregatedReceipts,
    /// Last sent RAV
    ///
    /// This is used to together with a list of receipts to aggregate
    /// into a new RAV
    latest_rav: Option<Eip712SignedMessage<T::Rav>>,
    /// Database connection
    pgpool: PgPool,
    /// Instance of TapManager for our [NetworkVersion] T
    tap_manager: TapManager<T>,
    /// Current allocation/collection identifier
    allocation_id: T::AllocationId,
    /// Address of the sender responsible for this [SenderAllocation]
    sender: Address,
    /// Address of the indexer
    indexer_address: Address,

    /// Watcher containing the escrow accounts
    escrow_accounts: Receiver<EscrowAccounts>,
    /// Domain separator used for tap
    domain_separator: Eip712Domain,
    /// Reference to [super::sender_account::SenderAccount] actor
    ///
    /// This is needed to return back Rav responses
    sender_account_ref: ActorRef<SenderAccountMessage>,
    /// Aggregator client
    ///
    /// This is defined by [NetworkVersion::AggregatorClient] depending
    /// if it's a [crate::tap::context::Legacy] or a [crate::tap::context::Horizon] version
    sender_aggregator: T::AggregatorClient,
    /// Buffer configuration used by TAP so gives some room to receive receipts
    /// that are delayed since timestamp_ns is defined by the gateway
    timestamp_buffer_ns: u64,
    /// Limit of receipts sent in a Rav Request
    rav_request_receipt_limit: u64,
}

/// Configuration derived from config.toml
#[derive(Clone)]
pub struct AllocationConfig {
    /// Buffer used for the receipts
    pub timestamp_buffer_ns: u64,
    /// Limit of receipts sent in a Rav Request
    pub rav_request_receipt_limit: u64,
    /// Current indexer address
    pub indexer_address: Address,
    /// Polling interval for escrow subgraph
    pub escrow_polling_interval: Duration,
}

impl AllocationConfig {
    /// Creates a [SenderAccountConfig] by getting a reference of [super::sender_account::SenderAccountConfig]
    pub fn from_sender_config(config: &SenderAccountConfig) -> Self {
        Self {
            timestamp_buffer_ns: config.rav_request_buffer.as_nanos() as u64,
            rav_request_receipt_limit: config.rav_request_receipt_limit,
            indexer_address: config.indexer_address,
            escrow_polling_interval: config.escrow_polling_interval,
        }
    }
}

/// Arguments used to initialize [SenderAllocation]
#[derive(bon::Builder)]
pub struct SenderAllocationArgs<T: NetworkVersion> {
    /// Database connection
    pub pgpool: PgPool,
    /// Current allocation/collection identifier
    pub allocation_id: T::AllocationId,
    /// Address of the sender responsible for this [SenderAllocation]
    pub sender: Address,
    /// Watcher containing the escrow accounts
    pub escrow_accounts: Receiver<EscrowAccounts>,
    /// SubgraphClient of the escrow subgraph
    pub escrow_subgraph: &'static SubgraphClient,
    /// Domain separator used for tap
    pub domain_separator: Eip712Domain,
    /// Reference to [super::sender_account::SenderAccount] actor
    ///
    /// This is needed to return back Rav responses
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
    /// Aggregator client
    ///
    /// This is defined by [crate::tap::context::NetworkVersion::AggregatorClient] depending
    /// if it's a [crate::tap::context::Legacy] or a [crate::tap::context::Horizon] version
    pub sender_aggregator: T::AggregatorClient,

    /// General configuration from config.toml
    pub config: AllocationConfig,
}

/// Enum containing all types of messages that a [SenderAllocation] can receive
#[derive(Debug)]
#[cfg_attr(any(test, feature = "test"), derive(educe::Educe))]
#[cfg_attr(any(test, feature = "test"), educe(Clone))]
pub enum SenderAllocationMessage {
    /// New receipt message, sent by the task spawned by
    /// [super::sender_accounts_manager::SenderAccountsManager]
    NewReceipt(NewReceiptNotification),
    /// Triggers a Rav Request for the current allocation
    ///
    /// It notifies its parent with the response
    TriggerRavRequest,
    #[cfg(any(test, feature = "test"))]
    /// Return the internal state (used for tests)
    GetUnaggregatedReceipts(
        #[educe(Clone(method(crate::test::actors::clone_rpc_reply)))]
        ractor::RpcReplyPort<UnaggregatedReceipts>,
    ),
}

/// Actor implementation for [SenderAllocation]
///
/// We use some bounds so [TapAgentContext] implements all parts needed for the given
/// [crate::tap::context::NetworkVersion]
#[async_trait::async_trait]
impl<T> Actor for SenderAllocation<T>
where
    SenderAllocationState<T>: DatabaseInteractions,
    T: NetworkVersion,
    for<'a> &'a Eip712SignedMessage<T::Rav>: Into<RavInformation>,
    TapAgentContext<T>:
        RavRead<T::Rav> + RavStore<T::Rav> + ReceiptDelete + ReceiptRead<TapReceipt>,
{
    type Msg = SenderAllocationMessage;
    type State = SenderAllocationState<T>;
    type Arguments = SenderAllocationArgs<T>;

    /// This is called in the [ractor::Actor::spawn] method and is used
    /// to process the [SenderAllocationArgs] with a reference to the current
    /// actor
    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let sender_account_ref = args.sender_account_ref.clone();
        let allocation_id = args.allocation_id.clone();
        let mut state = SenderAllocationState::new(args).await?;

        // update invalid receipts
        state.invalid_receipts_fees = state.calculate_invalid_receipts_fee().await?;
        if state.invalid_receipts_fees.value > 0 {
            sender_account_ref.cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                T::to_allocation_id_enum(&allocation_id),
                state.invalid_receipts_fees,
            ))?;
        }

        // update unaggregated_fees
        state.unaggregated_fees = state.recalculate_all_unaggregated_fees().await?;

        sender_account_ref.cast(SenderAccountMessage::UpdateReceiptFees(
            T::to_allocation_id_enum(&allocation_id),
            ReceiptFees::UpdateValue(state.unaggregated_fees),
        ))?;

        // update rav tracker for sender account
        if let Some(rav) = &state.latest_rav {
            sender_account_ref.cast(SenderAccountMessage::UpdateRav(rav.into()))?;
        }

        tracing::info!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            "SenderAllocation created!",
        );

        Ok(state)
    }

    /// This method only runs on graceful stop (real close allocation)
    /// if the actor crashes, this is not ran
    ///
    /// It's used to flush all remaining receipts while creating Ravs
    /// and marking it as last to be redeemed by indexer-agent
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            "Closing SenderAllocation, triggering last rav",
        );
        loop {
            match state.recalculate_all_unaggregated_fees().await {
                Ok(value) => {
                    state.unaggregated_fees = value;
                    break;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "There was an error while calculating the last unaggregated receipts. Retrying in 30 seconds...");
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        }
        // Request a RAV and mark the allocation as final.
        while state.unaggregated_fees.value > 0 {
            if let Err(err) = state.request_rav().await {
                tracing::error!(error = %err, "There was an error while requesting rav. Retrying in 30 seconds...");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }

        while let Err(err) = state.mark_rav_last().await {
            tracing::error!(
                error = %err,
                %state.allocation_id,
                %state.sender,
                "Error while marking allocation last. Retrying in 30 seconds..."
            );
            tokio::time::sleep(Duration::from_secs(30)).await;
        }

        // Since this is only triggered after allocation is closed will be counted here
        CLOSED_SENDER_ALLOCATIONS
            .with_label_values(&[&state.sender.to_string()])
            .inc();

        Ok(())
    }

    /// Handle a new [SenderAllocationMessage] message
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::trace!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            ?message,
            "New SenderAllocation message"
        );
        let unaggregated_fees = &mut state.unaggregated_fees;

        match message {
            SenderAllocationMessage::NewReceipt(notification) => {
                let NewReceiptNotification {
                    id,
                    value: fees,
                    timestamp_ns,
                    ..
                } = notification;
                if id <= unaggregated_fees.last_id {
                    // Unexpected: received a receipt with an ID not greater than the last processed one
                    tracing::warn!(
                        unaggregated_fees_last_id = %unaggregated_fees.last_id,
                        last_id = %id,
                        "Received a receipt notification that was already calculated."
                    );
                    return Ok(());
                }
                unaggregated_fees.last_id = id;
                unaggregated_fees.value =
                    unaggregated_fees
                        .value
                        .checked_add(fees)
                        .unwrap_or_else(|| {
                            // This should never happen, but if it does, we want to know about it.
                            tracing::error!(
                            "Overflow when adding receipt value {} to total unaggregated fees {} \
                            for allocation {} and sender {}. Setting total unaggregated fees to \
                            u128::MAX.",
                            fees, unaggregated_fees.value, state.allocation_id, state.sender
                        );
                            u128::MAX
                        });
                unaggregated_fees.counter += 1;
                // it's fine to crash the actor, could not send a message to its parent
                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        T::to_allocation_id_enum(&state.allocation_id),
                        ReceiptFees::NewReceipt(fees, timestamp_ns),
                    ))?;
            }
            SenderAllocationMessage::TriggerRavRequest => {
                let rav_result = if state.unaggregated_fees.value > 0 {
                    state.request_rav().await.map(|_| state.latest_rav.as_ref())
                } else {
                    Err(anyhow!("Unaggregated fee equals zero"))
                };
                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        T::to_allocation_id_enum(&state.allocation_id),
                        ReceiptFees::RavRequestResponse(
                            state.unaggregated_fees,
                            rav_result.map(|res| res.map(Into::into)),
                        ),
                    ))?;
            }
            #[cfg(any(test, feature = "test"))]
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(*unaggregated_fees);
                }
            }
        }

        Ok(())
    }
}

/// We use some bounds so [TapAgentContext] implements all parts needed for the given
/// [crate::tap::context::NetworkVersion]
impl<T> SenderAllocationState<T>
where
    T: NetworkVersion,
    TapAgentContext<T>:
        RavRead<T::Rav> + RavStore<T::Rav> + ReceiptDelete + ReceiptRead<TapReceipt>,
    SenderAllocationState<T>: DatabaseInteractions,
{
    /// Helper function to create a [SenderAllocationState]
    /// given [SenderAllocationArgs]
    async fn new(
        SenderAllocationArgs {
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            escrow_subgraph,
            domain_separator,
            sender_account_ref,
            sender_aggregator,
            config,
        }: SenderAllocationArgs<T>,
    ) -> anyhow::Result<Self> {
        let required_checks: Vec<Arc<dyn Check<TapReceipt> + Send + Sync>> = vec![
            Arc::new(
                AllocationId::new(
                    config.indexer_address,
                    config.escrow_polling_interval,
                    sender,
                    T::allocation_id_to_address(&allocation_id),
                    escrow_subgraph,
                )
                .await,
            ),
            Arc::new(Signature::new(
                domain_separator.clone(),
                escrow_accounts.clone(),
            )),
        ];
        let context = TapAgentContext::builder()
            .pgpool(pgpool.clone())
            .allocation_id(T::allocation_id_to_address(&allocation_id))
            .indexer_address(config.indexer_address)
            .sender(sender)
            .escrow_accounts(escrow_accounts.clone())
            .build();

        let latest_rav = context.last_rav().await.unwrap_or_default();
        let tap_manager = TapManager::new(
            domain_separator.clone(),
            context,
            CheckList::new(required_checks),
        );

        Ok(Self {
            pgpool,
            tap_manager,
            allocation_id,
            sender,
            escrow_accounts,
            domain_separator,
            indexer_address: config.indexer_address,
            sender_account_ref: sender_account_ref.clone(),
            unaggregated_fees: UnaggregatedReceipts::default(),
            invalid_receipts_fees: UnaggregatedReceipts::default(),
            latest_rav,
            sender_aggregator,
            rav_request_receipt_limit: config.rav_request_receipt_limit,
            timestamp_buffer_ns: config.timestamp_buffer_ns,
        })
    }

    async fn recalculate_all_unaggregated_fees(&self) -> anyhow::Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(i64::MAX).await
    }

    async fn calculate_unaggregated_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(self.unaggregated_fees.last_id as i64)
            .await
    }

    async fn request_rav(&mut self) -> anyhow::Result<()> {
        match self.rav_requester_single().await {
            Ok(rav) => {
                self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                self.latest_rav = Some(rav);
                RAVS_CREATED
                    .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
                    .inc();
                Ok(())
            }
            Err(e) => {
                if let RavError::AllReceiptsInvalid = e {
                    self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                }
                RAVS_FAILED
                    .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
                    .inc();
                Err(e.into())
            }
        }
    }

    /// Request a RAV from the sender's TAP aggregator. Only one RAV request will be running at a
    /// time because actors run one message at a time.
    ///
    /// Yet, multiple different [SenderAllocation] can run a request in parallel.
    async fn rav_requester_single(&mut self) -> Result<Eip712SignedMessage<T::Rav>, RavError> {
        tracing::trace!("rav_requester_single()");
        let RavRequest {
            valid_receipts,
            previous_rav,
            invalid_receipts,
            expected_rav,
        } = self
            .tap_manager
            .create_rav_request(
                &Context::new(),
                self.timestamp_buffer_ns,
                Some(self.rav_request_receipt_limit),
            )
            .await?;
        match (
            expected_rav,
            valid_receipts.is_empty(),
            invalid_receipts.is_empty(),
        ) {
            // All receipts are invalid
            (Err(AggregationError::NoValidReceiptsForRavRequest), true, false) => {
                tracing::warn!(
                    "Found {} invalid receipts for allocation {} and sender {}.",
                    invalid_receipts.len(),
                    self.allocation_id,
                    self.sender
                );
                // Obtain min/max timestamps to define query
                let min_timestamp = invalid_receipts
                    .iter()
                    .map(|receipt| receipt.signed_receipt().timestamp_ns())
                    .min()
                    .expect("invalid receipts should not be empty");
                let max_timestamp = invalid_receipts
                    .iter()
                    .map(|receipt| receipt.signed_receipt().timestamp_ns())
                    .max()
                    .expect("invalid receipts should not be empty");

                self.store_invalid_receipts(invalid_receipts).await?;
                let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;
                self.delete_receipts_between(&signers, min_timestamp, max_timestamp)
                    .await?;
                Err(RavError::AllReceiptsInvalid)
            }
            // When it receives both valid and invalid receipts or just valid
            (Ok(expected_rav), ..) => {
                let valid_receipts: Vec<_> = valid_receipts
                    .into_iter()
                    .map(|r| r.signed_receipt().clone())
                    .collect();

                let rav_response_time_start = Instant::now();

                let signed_rav =
                    T::aggregate(&mut self.sender_aggregator, valid_receipts, previous_rav).await?;

                let rav_response_time = rav_response_time_start.elapsed();
                RAV_RESPONSE_TIME
                    .with_label_values(&[&self.sender.to_string()])
                    .observe(rav_response_time.as_secs_f64());
                // we only save invalid receipts when we are about to store our rav
                //
                // store them before we call remove_obsolete_receipts()
                if !invalid_receipts.is_empty() {
                    tracing::warn!(
                        "Found {} invalid receipts for allocation {} and sender {}.",
                        invalid_receipts.len(),
                        self.allocation_id,
                        self.sender
                    );

                    // Save invalid receipts to the database for logs.
                    // TODO: consider doing that in a spawned task?
                    self.store_invalid_receipts(invalid_receipts).await?;
                }

                match self
                    .tap_manager
                    .verify_and_store_rav(expected_rav.clone(), signed_rav.clone())
                    .await
                {
                    Ok(_) => {}

                    // Adapter errors are local software errors. Shouldn't be a problem with the sender.
                    Err(tap_core::Error::AdapterError { source_error: e }) => {
                        return Err(
                            anyhow::anyhow!("TAP Adapter error while storing RAV: {:?}", e).into(),
                        )
                    }

                    // The 3 errors below signal an invalid RAV, which should be about problems with the
                    // sender. The sender could be malicious.
                    Err(
                        e @ tap_core::Error::InvalidReceivedRav {
                            expected_rav: _,
                            received_rav: _,
                        }
                        | e @ tap_core::Error::SignatureError(_)
                        | e @ tap_core::Error::InvalidRecoveredSigner { address: _ },
                    ) => {
                        Self::store_failed_rav(self, &expected_rav, &signed_rav, &e.to_string())
                            .await?;
                        return Err(anyhow::anyhow!(
                            "Invalid RAV, sender could be malicious: {:?}.",
                            e
                        )
                        .into());
                    }

                    // All relevant errors should be handled above. If we get here, we forgot to handle
                    // an error case.
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error while verifying and storing RAV: {:?}",
                            e
                        )
                        .into());
                    }
                }
                Ok(signed_rav)
            }
            (Err(AggregationError::NoValidReceiptsForRavRequest), true, true) => Err(anyhow!(
                "It looks like there are no valid receipts for the RAV request.\
                This may happen if your `rav_request_trigger_value` is too low \
                and no receipts were found outside the `rav_request_timestamp_buffer_ms`.\
                You can fix this by increasing the `rav_request_trigger_value`."
            )
            .into()),
            (Err(e), ..) => Err(e.into()),
        }
    }

    async fn store_invalid_receipts(
        &mut self,
        receipts: Vec<ReceiptWithState<Failed, TapReceipt>>,
    ) -> anyhow::Result<()> {
        let fees = receipts
            .iter()
            .map(|receipt| receipt.signed_receipt().value())
            .sum();

        let (receipts_v1, receipts_v2): (Vec<_>, Vec<_>) =
            receipts.into_iter().partition_map(|r| {
                // note: it would be nice if we could get signed_receipt and error by value without
                // cloning
                let error = r.clone().error().to_string();
                match r.signed_receipt().clone() {
                    TapReceipt::V1(receipt) => Either::Left((receipt, error)),
                    TapReceipt::V2(receipt) => Either::Right((receipt, error)),
                }
            });

        let (result1, result2) = tokio::join!(
            self.store_v1_invalid_receipts(receipts_v1),
            self.store_v2_invalid_receipts(receipts_v2),
        );
        if let Err(err) = result1 {
            tracing::error!(%err, "There was an error while storing invalid v1 receipts.");
        }

        if let Err(err) = result2 {
            tracing::error!(%err, "There was an error while storing invalid v2 receipts.");
        }

        self.invalid_receipts_fees.value = self
            .invalid_receipts_fees
            .value
            .checked_add(fees)
            .unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                tracing::error!(
                    "Overflow when adding receipt value {} to invalid receipts fees {} \
            for allocation {} and sender {}. Setting total unaggregated fees to \
            u128::MAX.",
                    fees,
                    self.invalid_receipts_fees.value,
                    self.allocation_id,
                    self.sender
                );
                u128::MAX
            });
        self.sender_account_ref
            .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                T::to_allocation_id_enum(&self.allocation_id),
                self.invalid_receipts_fees,
            ))?;

        Ok(())
    }

    async fn store_v1_invalid_receipts(
        &self,
        receipts: Vec<(tap_graph::SignedReceipt, String)>,
    ) -> anyhow::Result<()> {
        let reciepts_len = receipts.len();
        let mut reciepts_signers = Vec::with_capacity(reciepts_len);
        let mut encoded_signatures = Vec::with_capacity(reciepts_len);
        let mut allocation_ids = Vec::with_capacity(reciepts_len);
        let mut timestamps = Vec::with_capacity(reciepts_len);
        let mut nounces = Vec::with_capacity(reciepts_len);
        let mut values = Vec::with_capacity(reciepts_len);
        let mut error_logs = Vec::with_capacity(reciepts_len);

        for (receipt, receipt_error) in receipts {
            let allocation_id = receipt.message.allocation_id;
            let encoded_signature = receipt.signature.as_bytes().to_vec();
            let receipt_signer = receipt
                .recover_signer(&self.domain_separator)
                .map_err(|e| {
                    tracing::error!("Failed to recover receipt signer: {}", e);
                    anyhow!(e)
                })?;
            tracing::debug!(
                "Receipt for allocation {} and signer {} failed reason: {}",
                allocation_id.encode_hex(),
                receipt_signer.encode_hex(),
                receipt_error
            );
            reciepts_signers.push(receipt_signer.encode_hex());
            encoded_signatures.push(encoded_signature);
            allocation_ids.push(allocation_id.encode_hex());
            timestamps.push(BigDecimal::from(receipt.message.timestamp_ns));
            nounces.push(BigDecimal::from(receipt.message.nonce));
            values.push(BigDecimal::from(BigInt::from(receipt.message.value)));
            error_logs.push(receipt_error);
        }
        sqlx::query!(
            r#"INSERT INTO scalar_tap_receipts_invalid (
                signer_address,
                signature,
                allocation_id,
                timestamp_ns,
                nonce,
                value,
                error_log
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(40)[],
                $4::NUMERIC(20)[],
                $5::NUMERIC(20)[],
                $6::NUMERIC(40)[],
                $7::TEXT[]
            )"#,
            &reciepts_signers,
            &encoded_signatures,
            &allocation_ids,
            &timestamps,
            &nounces,
            &values,
            &error_logs
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to store invalid receipt: {}", e);
            anyhow!(e)
        })?;

        Ok(())
    }

    async fn store_v2_invalid_receipts(
        &self,
        receipts: Vec<(tap_graph::v2::SignedReceipt, String)>,
    ) -> anyhow::Result<()> {
        let reciepts_len = receipts.len();
        let mut reciepts_signers = Vec::with_capacity(reciepts_len);
        let mut encoded_signatures = Vec::with_capacity(reciepts_len);
        let mut collection_ids = Vec::with_capacity(reciepts_len);
        let mut payers = Vec::with_capacity(reciepts_len);
        let mut data_services = Vec::with_capacity(reciepts_len);
        let mut service_providers = Vec::with_capacity(reciepts_len);
        let mut timestamps = Vec::with_capacity(reciepts_len);
        let mut nonces = Vec::with_capacity(reciepts_len);
        let mut values = Vec::with_capacity(reciepts_len);
        let mut error_logs = Vec::with_capacity(reciepts_len);

        for (receipt, receipt_error) in receipts {
            let collection_id = receipt.message.collection_id;
            let payer = receipt.message.payer;
            let data_service = receipt.message.data_service;
            let service_provider = receipt.message.service_provider;
            let encoded_signature = receipt.signature.as_bytes().to_vec();
            let receipt_signer = receipt
                .recover_signer(&self.domain_separator)
                .map_err(|e| {
                    tracing::error!("Failed to recover receipt signer: {}", e);
                    anyhow!(e)
                })?;
            tracing::debug!(
                "Receipt for allocation {} and signer {} failed reason: {}",
                collection_id.encode_hex(),
                receipt_signer.encode_hex(),
                receipt_error
            );
            reciepts_signers.push(receipt_signer.encode_hex());
            encoded_signatures.push(encoded_signature);
            collection_ids.push(collection_id.encode_hex());
            payers.push(payer.encode_hex());
            data_services.push(data_service.encode_hex());
            service_providers.push(service_provider.encode_hex());
            timestamps.push(BigDecimal::from(receipt.message.timestamp_ns));
            nonces.push(BigDecimal::from(receipt.message.nonce));
            values.push(BigDecimal::from(BigInt::from(receipt.message.value)));
            error_logs.push(receipt_error);
        }
        sqlx::query!(
            r#"INSERT INTO tap_horizon_receipts_invalid (
                signer_address,
                signature,
                collection_id,
                payer,
                data_service,
                service_provider,
                timestamp_ns,
                nonce,
                value,
                error_log
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(64)[],
                $4::CHAR(40)[],
                $5::CHAR(40)[],
                $6::CHAR(40)[],
                $7::NUMERIC(20)[],
                $8::NUMERIC(20)[],
                $9::NUMERIC(40)[],
                $10::TEXT[]
            )"#,
            &reciepts_signers,
            &encoded_signatures,
            &collection_ids,
            &payers,
            &data_services,
            &service_providers,
            &timestamps,
            &nonces,
            &values,
            &error_logs
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to store invalid receipt: {}", e);
            anyhow!(e)
        })?;

        Ok(())
    }

    /// Stores a failed Rav, used for logging purposes
    async fn store_failed_rav(
        &self,
        expected_rav: &T::Rav,
        rav: &Eip712SignedMessage<T::Rav>,
        reason: &str,
    ) -> anyhow::Result<()> {
        // Failed Ravs are stored as json, we don't need to have a copy of the table
        // TODO update table name?
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_rav_requests_failed (
                    allocation_id,
                    sender_address,
                    expected_rav,
                    rav_response,
                    reason
                )
                VALUES ($1, $2, $3, $4, $5)
            "#,
            T::allocation_id_to_address(&self.allocation_id).encode_hex(),
            self.sender.encode_hex(),
            serde_json::to_value(expected_rav)?,
            serde_json::to_value(rav)?,
            reason
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| anyhow!("Failed to store failed RAV: {:?}", e))?;

        Ok(())
    }
}

/// Interactions with the database that needs some special treatment depending on the NetworkVersion
pub trait DatabaseInteractions {
    /// Delete receipts between `min_timestamp` and `max_timestamp`
    fn delete_receipts_between(
        &self,
        signers: &[String],
        min_timestamp: u64,
        max_timestamp: u64,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Calculates fees for invalid receipts
    fn calculate_invalid_receipts_fee(
        &self,
    ) -> impl Future<Output = anyhow::Result<UnaggregatedReceipts>> + Send;

    /// Calculates all receipt fees until provided `last_id`
    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    fn calculate_fee_until_last_id(
        &self,
        last_id: i64,
    ) -> impl Future<Output = anyhow::Result<UnaggregatedReceipts>> + Send;

    /// Sends a database query and mark the allocation rav as last
    fn mark_rav_last(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

impl DatabaseInteractions for SenderAllocationState<Legacy> {
    async fn delete_receipts_between(
        &self,
        signers: &[String],
        min_timestamp: u64,
        max_timestamp: u64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            r#"
                        DELETE FROM scalar_tap_receipts
                        WHERE timestamp_ns BETWEEN $1 AND $2
                        AND allocation_id = $3
                        AND signer_address IN (SELECT unnest($4::text[]));
                    "#,
            BigDecimal::from(min_timestamp),
            BigDecimal::from(max_timestamp),
            (**self.allocation_id).encode_hex(),
            &signers,
        )
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }
    async fn calculate_invalid_receipts_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_invalid_receipts_fee()");
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;

        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                scalar_tap_receipts_invalid
            WHERE
                allocation_id = $1
                AND signer_address IN (SELECT unnest($2::text[]))
            "#,
            (**self.allocation_id).encode_hex(),
            &signers
        )
        .fetch_one(&self.pgpool)
        .await?;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_fee_until_last_id(
        &self,
        last_id: i64,
    ) -> anyhow::Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_unaggregated_fee()");
        self.tap_manager.remove_obsolete_receipts().await?;

        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;
        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                scalar_tap_receipts
            WHERE
                allocation_id = $1
                AND id <= $2
                AND signer_address IN (SELECT unnest($3::text[]))
                AND timestamp_ns > $4
            "#,
            (**self.allocation_id).encode_hex(),
            last_id,
            &signers,
            BigDecimal::from(
                self.latest_rav
                    .as_ref()
                    .map(|rav| rav.message.timestamp_ns())
                    .unwrap_or_default()
            ),
        )
        .fetch_one(&self.pgpool)
        .await?;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    /// Sends a database query and mark the allocation rav as last
    async fn mark_rav_last(&self) -> anyhow::Result<()> {
        tracing::info!(
            sender = %self.sender,
            allocation_id = %self.allocation_id,
            "Marking rav as last!",
        );
        let updated_rows = sqlx::query!(
            r#"
                        UPDATE scalar_tap_ravs
                        SET last = true
                        WHERE allocation_id = $1 AND sender_address = $2
                    "#,
            (**self.allocation_id).encode_hex(),
            self.sender.encode_hex(),
        )
        .execute(&self.pgpool)
        .await?;

        match updated_rows.rows_affected() {
            // in case no rav was marked as final
            0 => {
                tracing::warn!(
                    "No RAVs were updated as last for allocation {} and sender {}.",
                    self.allocation_id,
                    self.sender
                );
                Ok(())
            }
            1 => Ok(()),
            _ => anyhow::bail!(
                "Expected exactly one row to be updated in the latest RAVs table, \
                        but {} were updated.",
                updated_rows.rows_affected()
            ),
        }
    }
}

impl DatabaseInteractions for SenderAllocationState<Horizon> {
    async fn delete_receipts_between(
        &self,
        signers: &[String],
        min_timestamp: u64,
        max_timestamp: u64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            r#"
                        DELETE FROM scalar_tap_receipts
                        WHERE timestamp_ns BETWEEN $1 AND $2
                        AND allocation_id = $3
                        AND signer_address IN (SELECT unnest($4::text[]));
                    "#,
            BigDecimal::from(min_timestamp),
            BigDecimal::from(max_timestamp),
            self.allocation_id.as_address().encode_hex(),
            &signers,
        )
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }

    async fn calculate_invalid_receipts_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_invalid_receipts_fee()");
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;

        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                tap_horizon_receipts_invalid
            WHERE
                collection_id = $1
                AND signer_address IN (SELECT unnest($2::text[]))
            "#,
            self.allocation_id.to_string(),
            &signers
        )
        .fetch_one(&self.pgpool)
        .await?;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    async fn calculate_fee_until_last_id(
        &self,
        last_id: i64,
    ) -> anyhow::Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_unaggregated_fee()");
        self.tap_manager.remove_obsolete_receipts().await?;

        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;
        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                tap_horizon_receipts
            WHERE
                collection_id = $1
                AND service_provider = $2
                AND id <= $3
                AND signer_address IN (SELECT unnest($4::text[]))
                AND timestamp_ns > $5
            "#,
            self.allocation_id.to_string(),
            self.indexer_address.encode_hex(),
            last_id,
            &signers,
            BigDecimal::from(
                self.latest_rav
                    .as_ref()
                    .map(|rav| rav.message.timestamp_ns())
                    .unwrap_or_default()
            ),
        )
        .fetch_one(&self.pgpool)
        .await?;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    /// Sends a database query and mark the allocation rav as last
    async fn mark_rav_last(&self) -> anyhow::Result<()> {
        tracing::info!(
            sender = %self.sender,
            allocation_id = %self.allocation_id,
            "Marking rav as last!",
        );
        // TODO add service_provider filter
        let updated_rows = sqlx::query!(
            r#"
                UPDATE tap_horizon_ravs
                    SET last = true
                WHERE 
                    collection_id = $1
                    AND payer = $2
                    AND service_provider = $3
            "#,
            self.allocation_id.to_string(),
            self.sender.encode_hex(),
            self.indexer_address.encode_hex(),
        )
        .execute(&self.pgpool)
        .await?;

        match updated_rows.rows_affected() {
            // in case no rav was marked as final
            0 => {
                tracing::warn!(
                    "No RAVs were updated as last for allocation {} and sender {}.",
                    self.allocation_id,
                    self.sender
                );
                Ok(())
            }
            1 => Ok(()),
            _ => anyhow::bail!(
                "Expected exactly one row to be updated in the latest RAVs table, \
                        but {} were updated.",
                updated_rows.rows_affected()
            ),
        }
    }
}

#[cfg(test)]
pub mod tests {
    #![allow(missing_docs)]
    use std::{
        collections::HashMap,
        future::Future,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use futures::future::join_all;
    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use indexer_receipt::TapReceipt;
    use ractor::{call, cast, Actor, ActorRef, ActorStatus};
    use ruint::aliases::U256;
    use serde_json::json;
    use serial_test::serial;
    use sqlx::PgPool;
    use tap_aggregator::grpc::v1::{tap_aggregator_client::TapAggregatorClient, RavResponse};
    use tap_core::receipt::{
        checks::{Check, CheckError, CheckList, CheckResult},
        Context,
    };
    use test_assets::{
        flush_messages, pgpool, ALLOCATION_ID_0, TAP_EIP712_DOMAIN as TAP_EIP712_DOMAIN_SEPARATOR,
        TAP_SENDER as SENDER, TAP_SIGNER as SIGNER,
    };
    use thegraph_core::AllocationId as AllocationIdCore;
    use tokio::sync::{mpsc, watch};
    use tonic::{transport::Endpoint, Code};
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockGuard, MockServer, ResponseTemplate,
    };
    use wiremock_grpc::{MockBuilder, Then};

    use super::{
        SenderAllocation, SenderAllocationArgs, SenderAllocationMessage, SenderAllocationState,
    };
    use crate::{
        agent::{
            sender_account::{ReceiptFees, SenderAccountMessage},
            sender_accounts_manager::{AllocationId, NewReceiptNotification},
            sender_allocation::DatabaseInteractions,
        },
        tap::{context::Legacy, CheckingReceipt},
        test::{
            actors::{create_mock_sender_account, TestableActor},
            create_rav, create_received_receipt, get_grpc_url, store_batch_receipts,
            store_invalid_receipt, store_rav, store_receipt, INDEXER,
        },
    };

    #[rstest::fixture]
    async fn mock_escrow_subgraph_server() -> (MockServer, MockGuard) {
        mock_escrow_subgraph().await
    }

    #[rstest::fixture]
    async fn state(
        #[future(awt)] pgpool: PgPool,
        #[future(awt)] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) -> SenderAllocationState<Legacy> {
        let (mock_escrow_subgraph_server, _mock_escrow_subgraph_guard) =
            mock_escrow_subgraph_server;
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
            .await;

        SenderAllocationState::new(args).await.unwrap()
    }

    async fn mock_escrow_subgraph() -> (MockServer, MockGuard) {
        let mock_ecrow_subgraph_server: MockServer = MockServer::start().await;
        let _mock_ecrow_subgraph = mock_ecrow_subgraph_server
                .register_as_scoped(
                    Mock::given(method("POST"))
                        .and(body_string_contains("TapTransactions"))
                        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                                "transactions": [{
                                    "id": "0x00224ee6ad4ae77b817b4e509dc29d644da9004ad0c44005a7f34481d421256409000000"
                                }],
                            }
                        }))),
                )
                .await;
        (mock_ecrow_subgraph_server, _mock_ecrow_subgraph)
    }
    #[bon::builder]
    async fn create_sender_allocation_args(
        pgpool: PgPool,
        sender_aggregator_endpoint: Option<String>,
        escrow_subgraph_endpoint: &str,
        #[builder(default = 1000)] rav_request_receipt_limit: u64,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> SenderAllocationArgs<Legacy> {
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
            )
            .await,
        ));

        let escrow_accounts_rx = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let sender_account_ref = match sender_account {
            Some(sender) => sender,
            None => create_mock_sender_account().await.1,
        };

        let aggregator_url = match sender_aggregator_endpoint {
            Some(url) => url,
            None => get_grpc_url().await,
        };

        let endpoint = Endpoint::new(aggregator_url).unwrap();

        let sender_aggregator = TapAggregatorClient::connect(endpoint.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "Failed to connect to the TapAggregator endpoint '{}': Err: {err:?}",
                    endpoint.uri()
                )
            });

        SenderAllocationArgs::builder()
            .pgpool(pgpool.clone())
            .allocation_id(AllocationIdCore::from(ALLOCATION_ID_0))
            .sender(SENDER.1)
            .escrow_accounts(escrow_accounts_rx)
            .escrow_subgraph(escrow_subgraph)
            .domain_separator(TAP_EIP712_DOMAIN_SEPARATOR.clone())
            .sender_account_ref(sender_account_ref)
            .sender_aggregator(sender_aggregator)
            .config(super::AllocationConfig {
                timestamp_buffer_ns: 1,
                rav_request_receipt_limit,
                indexer_address: INDEXER.1,
                escrow_polling_interval: Duration::from_millis(1000),
            })
            .build()
    }

    #[bon::builder]
    async fn create_sender_allocation(
        pgpool: PgPool,
        sender_aggregator_endpoint: Option<String>,
        escrow_subgraph_endpoint: &str,
        #[builder(default = 1000)] rav_request_receipt_limit: u64,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> (
        ActorRef<SenderAllocationMessage>,
        mpsc::Receiver<SenderAllocationMessage>,
    ) {
        let args = create_sender_allocation_args()
            .pgpool(pgpool)
            .maybe_sender_aggregator_endpoint(sender_aggregator_endpoint)
            .escrow_subgraph_endpoint(escrow_subgraph_endpoint)
            .sender_account(sender_account.unwrap())
            .rav_request_receipt_limit(rav_request_receipt_limit)
            .call()
            .await;

        let (sender, msg_receiver) = mpsc::channel(10);
        let actor = TestableActor::new(SenderAllocation::default(), sender);

        let (allocation_ref, _join_handle) = Actor::spawn(None, actor, args).await.unwrap();

        (allocation_ref, msg_receiver)
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn should_update_unaggregated_fees_on_start(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let (mut last_message_emitted, sender_account) = create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        let last_message_emitted = last_message_emitted.recv().await.unwrap();
        insta::assert_debug_snapshot!(last_message_emitted);

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 55u128);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn should_return_invalid_receipts_on_startup(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_invalid_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        let update_invalid_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(update_invalid_msg);

        let last_message_emitted = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(last_message_emitted);

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_receive_new_receipt(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        let (sender_allocation, mut msg_receiver) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // should validate with id less than last_id
        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 0,
                value: 10,
                allocation_id: ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns: 0,
            })
        )
        .unwrap();

        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 1,
                value: 20,
                allocation_id: ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns,
            })
        )
        .unwrap();

        flush_messages(&mut msg_receiver).await;

        // should emit update aggregate fees message to sender account
        let startup_load_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(startup_load_msg);

        let last_message_emitted = message_receiver.recv().await.unwrap();
        let expected_message = SenderAccountMessage::UpdateReceiptFees(
            AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
            ReceiptFees::NewReceipt(20u128, timestamp_ns),
        );
        assert_eq!(last_message_emitted, expected_message);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_trigger_rav_request(pgpool: PgPool) {
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        // Add receipts to the database.

        for i in 0..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();

            // store a copy that should fail in the uniqueness test
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, mut msg_receiver_alloc) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&mut msg_receiver_alloc).await;

        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);

        let startup_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(startup_msg);

        // Check if the sender received invalid receipt fees
        let msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(msg);

        let updated_receipt_fees = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(updated_receipt_fees);
    }

    async fn execute<Fut>(pgpool: PgPool, populate: impl FnOnce(PgPool) -> Fut)
    where
        Fut: Future<Output = ()>,
    {
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        populate(pgpool.clone()).await;

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, mut msg_receiver_alloc) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .rav_request_receipt_limit(2000)
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&mut msg_receiver_alloc).await;

        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);

        let startup_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(startup_msg);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_several_receipts_rav_request(pgpool: PgPool) {
        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        execute(pgpool, |pgpool| async move {
            // Add receipts to the database.

            for i in 0..AMOUNT_OF_RECEIPTS {
                let receipt =
                    create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
                store_receipt(&pgpool, receipt.signed_receipt())
                    .await
                    .unwrap();
            }
        })
        .await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_several_receipts_batch_insert_rav_request(pgpool: PgPool) {
        // Add batch receipts to the database.
        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        execute(pgpool, |pgpool| async move {
            // Add receipts to the database.
            let mut receipts = Vec::with_capacity(1000);
            for i in 0..AMOUNT_OF_RECEIPTS {
                let receipt =
                    create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
                receipts.push(receipt);
            }
            let res = store_batch_receipts(&pgpool, receipts).await;
            assert!(res.is_ok());
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_close_allocation_no_pending_fees(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // create allocation
        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .sender_account(sender_account)
            .call()
            .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        // check if message is sent to sender account
        insta::assert_debug_snapshot!(message_receiver.recv().await);
    }

    // used for test_close_allocation_with_pending_fees(pgpool:
    mod wiremock_gen {
        wiremock_grpc::generate!("tap_aggregator.v1.TapAggregator", MockTapAggregator);
    }

    #[test_log::test(sqlx::test(migrations = "../../migrations"))]
    async fn test_close_allocation_with_pending_fees(pgpool: PgPool) {
        use wiremock_gen::MockTapAggregator;
        let mut mock_aggregator = MockTapAggregator::start_default().await;

        let request1 = mock_aggregator.setup(
            MockBuilder::when()
                //    👇 RPC prefix
                .path("/tap_aggregator.v1.TapAggregator/AggregateReceipts")
                .then()
                .return_status(Code::Ok)
                .return_body(|| {
                    let mock_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 10, 45);
                    RavResponse {
                        rav: Some(mock_rav.into()),
                    }
                }),
        );

        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (_, sender_account) = create_mock_sender_account().await;

        // create allocation
        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .sender_aggregator_endpoint(format!(
                "http://[::1]:{}",
                mock_aggregator.address().port()
            ))
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();

        // check if rav request was made
        assert!(mock_aggregator.find_request_count() > 0);
        assert!(mock_aggregator.find(&request1).is_some());

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn should_return_unaggregated_fees_without_rav(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // calculate unaggregated fee
        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn should_calculate_invalid_receipts_fee(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_invalid_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // calculate invalid unaggregated fee
        let total_invalid_receipts = state.calculate_invalid_receipts_fee().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_invalid_receipts.value, 45u128);
    }

    /// Test that the sender_allocation correctly updates the unaggregated fees from the
    /// database when there is a RAV in the database as well as receipts which timestamp are lesser
    /// and greater than the RAV's timestamp.
    ///
    /// The sender_allocation should only consider receipts with a timestamp greater
    /// than the RAV's timestamp.
    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn should_return_unaggregated_fees_with_rav(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();
        // Add the RAV to the database.
        // This RAV has timestamp 4. The sender_allocation should only consider receipts
        // with a timestamp greater than 4.
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 35u128);
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_store_failed_rav(#[future[awt]] state: SenderAllocationState<Legacy>) {
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);

        // just unit test if it is working
        let result = state
            .store_failed_rav(&signed_rav.message, &signed_rav, "test")
            .await;

        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_store_invalid_receipts(#[future[awt]] mut state: SenderAllocationState<Legacy>) {
        struct FailingCheck;

        #[async_trait::async_trait]
        impl Check<TapReceipt> for FailingCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                _receipt: &CheckingReceipt,
            ) -> CheckResult {
                Err(CheckError::Failed(anyhow::anyhow!("Failing check")))
            }
        }

        let checks = CheckList::new(vec![Arc::new(FailingCheck)]);

        // create some checks
        let checking_receipts = vec![
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
        ];
        // make sure to fail them
        let failing_receipts = checking_receipts
            .into_iter()
            .map(|receipt| async {
                receipt
                    .finalize_receipt_checks(&Context::new(), &checks)
                    .await
                    .unwrap()
                    .unwrap_err()
            })
            .collect::<Vec<_>>();
        let failing_receipts: Vec<_> = join_all(failing_receipts).await;

        // store the failing receipts
        let result = state.store_invalid_receipts(failing_receipts).await;

        // we just store a few and make sure it doesn't fail
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_mark_rav_last(#[future[awt]] state: SenderAllocationState<Legacy>) {
        // mark rav as final
        let result = state.mark_rav_last().await;

        // check if it fails
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_failed_rav_request(
        #[future(awt)] pgpool: PgPool,
        #[future[awt]] mock_escrow_subgraph_server: (MockServer, MockGuard),
    ) {
        // Add receipts to the database.
        for i in 0..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, u64::MAX, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, mut notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.0.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&mut notify).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(startup_msg);

        let rav_error_response_message = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(rav_error_response_message);

        // expect the actor to keep running
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

        // Check that the unaggregated fees return the same value
        // TODO: Maybe this can no longer be checked?
        //assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_rav_request_when_all_receipts_invalid(pgpool: PgPool) {
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "transactions": [
                            {
                                "id": "redeemed"
                            }
                        ]}}),
                    )),
            )
            .await;
        // Add invalid receipts to the database. ( already redeemed )
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos() as u64;
        const RECEIPT_VALUE: u128 = 1622018441284756158;
        const TOTAL_RECEIPTS: u64 = 10;

        for i in 0..TOTAL_RECEIPTS {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, timestamp, RECEIPT_VALUE);
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        let (sender_allocation, mut notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&mut notify).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(startup_msg);

        let invalid_receipts = message_receiver.recv().await.unwrap();

        insta::assert_debug_snapshot!(invalid_receipts);

        let rav_error_response_message = message_receiver.recv().await.unwrap();
        insta::assert_debug_snapshot!(rav_error_response_message);

        let invalid_receipts = sqlx::query!(
            r#"
                SELECT * FROM scalar_tap_receipts_invalid;
            "#,
        )
        .fetch_all(&pgpool)
        .await
        .expect("Should not fail to fetch from scalar_tap_receipts_invalid");

        // Invalid receipts should be found inside the table
        assert_eq!(invalid_receipts.len(), 10);

        // make sure scalar_tap_receipts gets emptied
        let all_receipts = sqlx::query!(
            r#"
                SELECT * FROM scalar_tap_receipts;
            "#,
        )
        .fetch_all(&pgpool)
        .await
        .expect("Should not fail to fetch from scalar_tap_receipts");

        // Invalid receipts should be found inside the table
        assert!(all_receipts.is_empty());
    }
}
