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
            Horizon, NetworkVersion, TapAgentContext,
        },
        signers_trimmed, TapReceipt,
    },
};

pub(crate) static CLOSED_SENDER_ALLOCATIONS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_closed_sender_allocation_total",
        "Count of sender-allocation managers closed since the start of the program",
        &["sender"]
    )
    .unwrap()
});
pub(crate) static RAVS_CREATED_BY_VERSION: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_ravs_created_total_by_version",
        "RAVs created/updated per sender allocation and TAP version",
        &["sender", "allocation", "version"]
    )
    .unwrap()
});
pub(crate) static RAVS_FAILED_BY_VERSION: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_ravs_failed_total_by_version",
        "RAV requests failed per sender allocation and TAP version",
        &["sender", "allocation", "version"]
    )
    .unwrap()
});
pub(crate) static RAV_RESPONSE_TIME_BY_VERSION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "tap_rav_response_time_seconds_by_version",
        "RAV response time per sender and TAP version",
        &["sender", "version"]
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

const TAP_VERSION: &str = "v2";

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
    /// Domain separator used for Horizon receipts.
    /// TODO: Double check if we actually need to add an additional domain_sepparator_v2 field
    /// at first glance it seems like each sender allocation will deal only with one allocation
    /// type. not both
    domain_separator: Eip712Domain,
    /// Reference to [super::sender_account::SenderAccount] actor
    ///
    /// This is needed to return back Rav responses
    sender_account_ref: ActorRef<SenderAccountMessage>,
    /// Aggregator client for Horizon receipts.
    sender_aggregator: T::AggregatorClient,
    /// Buffer configuration used by TAP so gives some room to receive receipts
    /// that are delayed since timestamp_ns is defined by the gateway
    timestamp_buffer_ns: u64,
    /// Limit of receipts sent in a Rav Request
    rav_request_receipt_limit: u64,
    /// Data service address for Horizon mode from config
    data_service: Option<Address>,
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
    /// TAP protocol operation mode (Horizon mode required)
    pub tap_mode: indexer_config::TapMode,
}

impl AllocationConfig {
    /// Creates a [AllocationConfig] by getting a reference of [super::sender_account::SenderAccountConfig]
    pub fn from_sender_config(config: &SenderAccountConfig) -> Self {
        Self {
            timestamp_buffer_ns: config.rav_request_buffer.as_nanos() as u64,
            rav_request_receipt_limit: config.rav_request_receipt_limit,
            indexer_address: config.indexer_address,
            escrow_polling_interval: config.escrow_polling_interval,
            tap_mode: config.tap_mode.clone(),
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
    /// SubgraphClient of the network subgraph
    pub network_subgraph: &'static SubgraphClient,
    /// Domain separator used for tap
    pub domain_separator: Eip712Domain,
    /// Reference to [super::sender_account::SenderAccount] actor
    ///
    /// This is needed to return back Rav responses
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
    /// Aggregator client for Horizon receipts.
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
                        "Error calculating last unaggregated receipts; retrying in 30s",
                    );
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        }
        // Request a RAV and mark the allocation as final.
        while state.unaggregated_fees.value > 0 {
            if let Err(err) = state.request_rav().await {
                tracing::error!(
                    error = %err,
                    "Error requesting RAV; retrying in 30s",
                );
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
                let id = notification.id();
                let fees = notification.value();
                let timestamp_ns = notification.timestamp_ns();
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
                                fees,
                                current_total = unaggregated_fees.value,
                                allocation_id = %state.allocation_id,
                                sender = %state.sender,
                                "Overflow when adding receipt value; setting total unaggregated fees to u128::MAX",
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
            network_subgraph,
            domain_separator,
            sender_account_ref,
            sender_aggregator,
            config,
        }: SenderAllocationArgs<T>,
    ) -> anyhow::Result<Self> {
        let collection_id = T::to_allocation_id_enum(&allocation_id).0;
        let required_checks: Vec<Arc<dyn Check<TapReceipt> + Send + Sync>> = vec![
            Arc::new(
                AllocationId::new(
                    config.indexer_address,
                    config.escrow_polling_interval,
                    sender,
                    T::allocation_id_to_address(&allocation_id),
                    collection_id,
                    network_subgraph,
                )
                .await,
            ),
            Arc::new(Signature::new(
                domain_separator.clone(),
                escrow_accounts.clone(),
            )),
        ];
        let subgraph_service_address = config
            .tap_mode
            .subgraph_service_address()
            .expect("Horizon mode required for sender allocation");
        let context = TapAgentContext::builder()
            .pgpool(pgpool.clone())
            .allocation_id(T::allocation_id_to_address(&allocation_id))
            .indexer_address(config.indexer_address)
            .sender(sender)
            .escrow_accounts(escrow_accounts.clone())
            .subgraph_service_address(subgraph_service_address)
            .build();

        let latest_rav = context.last_rav().await.unwrap_or_default();
        let tap_manager = TapManager::new(
            domain_separator.clone(),
            context,
            CheckList::new(required_checks),
        );

        let data_service = Some(subgraph_service_address);

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
            data_service,
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
                RAVS_CREATED_BY_VERSION
                    .with_label_values(&[
                        &self.sender.to_string(),
                        &self.allocation_id.to_string(),
                        TAP_VERSION,
                    ])
                    .inc();
                Ok(())
            }
            Err(e) => {
                if let RavError::AllReceiptsInvalid = e {
                    self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                }
                RAVS_FAILED_BY_VERSION
                    .with_label_values(&[
                        &self.sender.to_string(),
                        &self.allocation_id.to_string(),
                        TAP_VERSION,
                    ])
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
                    invalid_count = invalid_receipts.len(),
                    allocation_id = %self.allocation_id,
                    sender = %self.sender,
                    "Found invalid receipts",
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

                // Instrumentation: log details before calling the aggregator
                let receipt_count = valid_receipts.len();
                let first_signer = valid_receipts.first().and_then(|r| match r {
                    indexer_receipt::TapReceipt::V2(sr) => {
                        sr.recover_signer(&self.domain_separator).ok()
                    }
                    indexer_receipt::TapReceipt::V1(_) => None,
                });
                tracing::info!(
                    sender = %self.sender,
                    allocation_id = %self.allocation_id,
                    receipt_count,
                    has_previous_rav = previous_rav.is_some(),
                    signer_recovered = first_signer.is_some(),
                    agent_domain = ?self.domain_separator,
                    "Sending RAV aggregation request"
                );

                let rav_response_time_start = Instant::now();

                let signed_rav =
                    T::aggregate(&mut self.sender_aggregator, valid_receipts, previous_rav).await?;

                let rav_response_time = rav_response_time_start.elapsed();
                RAV_RESPONSE_TIME_BY_VERSION
                    .with_label_values(&[&self.sender.to_string(), TAP_VERSION])
                    .observe(rav_response_time.as_secs_f64());
                // we only save invalid receipts when we are about to store our rav
                //
                // store them before we call remove_obsolete_receipts()
                if !invalid_receipts.is_empty() {
                    tracing::warn!(
                        invalid_count = invalid_receipts.len(),
                        allocation_id = %self.allocation_id,
                        sender = %self.sender,
                        "Found invalid receipts",
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
            (Err(AggregationError::NoValidReceiptsForRavRequest), true, true) => {
                let table_name = "tap_horizon_receipts (V2/Horizon)";

                Err(anyhow!(
                    "It looks like there are no valid receipts for the RAV request from table: {}.\
                    This may happen if your `rav_request_trigger_value` is too low \
                    and no receipts were found outside the `rav_request_timestamp_buffer_ms`.\
                    You can fix this by increasing the `rav_request_trigger_value`.\
                    \nVerify receipts are in the Horizon receipts table for this allocation.",
                    table_name
                )
                .into())
            }
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

        let mut receipts_v2 = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            let error = receipt.clone().error().to_string();
            match receipt.signed_receipt().clone() {
                TapReceipt::V2(receipt) => receipts_v2.push((receipt, error)),
                TapReceipt::V1(_) => {
                    anyhow::bail!("V1 receipt encountered in Horizon-only mode");
                }
            }
        }

        if let Err(err) = self.store_v2_invalid_receipts(receipts_v2).await {
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
                    tracing::error!(error = %e, "Failed to recover receipt signer");
                    anyhow!(e)
                })?;
            tracing::debug!(
                collection_id = %collection_id.encode_hex(),
                signer = %receipt_signer.encode_hex(),
                reason = %receipt_error,
                "Invalid receipt stored",
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
        .map_err(|e: sqlx::Error| {
            tracing::error!(error = %e, "Failed to store invalid receipt");
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

impl DatabaseInteractions for SenderAllocationState<Horizon> {
    async fn delete_receipts_between(
        &self,
        signers: &[String],
        min_timestamp: u64,
        max_timestamp: u64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            r#"
                        DELETE FROM tap_horizon_receipts
                        WHERE timestamp_ns BETWEEN $1 AND $2
                        AND collection_id = $3
                        AND service_provider = $4
                        AND payer = $5
                        AND data_service = $6
                        AND signer_address IN (SELECT unnest($7::text[]));
                    "#,
            BigDecimal::from(min_timestamp),
            BigDecimal::from(max_timestamp),
            // self.allocation_id is already a CollectionId in Horizon state
            self.allocation_id.encode_hex(),
            self.indexer_address.encode_hex(),
            self.sender.encode_hex(),
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
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
                AND service_provider = $2
                AND payer = $3
                AND data_service = $4
                AND signer_address IN (SELECT unnest($5::text[]))
            "#,
            // self.allocation_id is already a CollectionId in Horizon state
            self.allocation_id.encode_hex(),
            self.indexer_address.encode_hex(),
            self.sender.encode_hex(),
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
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
                AND payer = $3
                AND data_service = $4
                AND id <= $5
                AND signer_address IN (SELECT unnest($6::text[]))
                AND timestamp_ns > $7
            "#,
            // self.allocation_id is already a CollectionId in Horizon state
            self.allocation_id.encode_hex(),
            self.indexer_address.encode_hex(),
            self.sender.encode_hex(),
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
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
                UPDATE tap_horizon_ravs
                    SET last = true
                WHERE 
                    collection_id = $1
                    AND payer = $2
                    AND service_provider = $3
                    AND data_service = $4
            "#,
            // self.allocation_id is already a CollectionId in Horizon state
            self.allocation_id.encode_hex(),
            self.sender.encode_hex(),
            self.indexer_address.encode_hex(),
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
        )
        .execute(&self.pgpool)
        .await?;

        match updated_rows.rows_affected() {
            // in case no rav was marked as final
            0 => {
                tracing::warn!(
                    allocation_id = %self.allocation_id,
                    sender = %self.sender,
                    "No RAVs were updated as last",
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

/// Force initialization of all LazyLock metrics in this module.
///
/// This ensures metrics are registered with Prometheus at startup,
/// even if no SenderAllocation actors have been created yet.
pub fn init_metrics() {
    // Dereference each LazyLock to force initialization
    let _ = &*CLOSED_SENDER_ALLOCATIONS;
    let _ = &*RAVS_CREATED_BY_VERSION;
    let _ = &*RAVS_FAILED_BY_VERSION;
    let _ = &*RAV_RESPONSE_TIME_BY_VERSION;
}
