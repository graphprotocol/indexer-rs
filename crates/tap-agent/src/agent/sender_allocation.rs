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
use sqlx::{types::BigDecimal, PgPool, Row};
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
        let subgraph_service_address = config.tap_mode.subgraph_service_address();
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
                let first_signer = valid_receipts
                    .first()
                    .and_then(|r| r.as_ref().recover_signer(&self.domain_separator).ok());
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
            let receipt = receipt.signed_receipt().0.clone();
            receipts_v2.push((receipt, error));
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
        let receipts_len = receipts.len();
        let mut reciepts_signers = Vec::with_capacity(receipts_len);
        let mut encoded_signatures = Vec::with_capacity(receipts_len);
        let mut collection_ids = Vec::with_capacity(receipts_len);
        let mut payers = Vec::with_capacity(receipts_len);
        let mut data_services = Vec::with_capacity(receipts_len);
        let mut service_providers = Vec::with_capacity(receipts_len);
        let mut timestamps = Vec::with_capacity(receipts_len);
        let mut nonces = Vec::with_capacity(receipts_len);
        let mut values = Vec::with_capacity(receipts_len);
        let mut error_logs = Vec::with_capacity(receipts_len);

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
        sqlx::query(
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
        )
        .bind(&reciepts_signers)
        .bind(&encoded_signatures)
        .bind(&collection_ids)
        .bind(&payers)
        .bind(&data_services)
        .bind(&service_providers)
        .bind(&timestamps)
        .bind(&nonces)
        .bind(&values)
        .bind(&error_logs)
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
        sqlx::query(
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
        )
        .bind(T::allocation_id_to_address(&self.allocation_id).encode_hex())
        .bind(self.sender.encode_hex())
        .bind(serde_json::to_value(expected_rav)?)
        .bind(serde_json::to_value(rav)?)
        .bind(reason)
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
        sqlx::query(
            r#"
                        DELETE FROM tap_horizon_receipts
                        WHERE timestamp_ns BETWEEN $1 AND $2
                        AND collection_id = $3
                        AND service_provider = $4
                        AND payer = $5
                        AND data_service = $6
                        AND signer_address IN (SELECT unnest($7::text[]));
                    "#,
        )
        .bind(BigDecimal::from(min_timestamp))
        .bind(BigDecimal::from(max_timestamp))
        // self.allocation_id is already a CollectionId in Horizon state
        .bind(self.allocation_id.encode_hex())
        .bind(self.indexer_address.encode_hex())
        .bind(self.sender.encode_hex())
        .bind(
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
        )
        .bind(signers)
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }

    async fn calculate_invalid_receipts_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_invalid_receipts_fee()");
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;

        let res = sqlx::query(
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
        )
        // self.allocation_id is already a CollectionId in Horizon state
        .bind(self.allocation_id.encode_hex())
        .bind(self.indexer_address.encode_hex())
        .bind(self.sender.encode_hex())
        .bind(
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
        )
        .bind(signers)
        .fetch_one(&self.pgpool)
        .await?;

        let max: Option<i64> = res.try_get("max")?;
        let sum: Option<BigDecimal> = res.try_get("sum")?;
        let count: Option<i64> = res.try_get("count")?;

        ensure!(
            sum.is_none() == max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: max.unwrap_or(0).try_into()?,
            value: sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: count
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
        let res = sqlx::query(
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
        )
        // self.allocation_id is already a CollectionId in Horizon state
        .bind(self.allocation_id.encode_hex())
        .bind(self.indexer_address.encode_hex())
        .bind(self.sender.encode_hex())
        .bind(
            self.data_service
                .expect("data_service should be available in Horizon mode")
                .encode_hex(),
        )
        .bind(last_id)
        .bind(signers)
        .bind(BigDecimal::from(
            self.latest_rav
                .as_ref()
                .map(|rav| rav.message.timestamp_ns())
                .unwrap_or_default(),
        ))
        .fetch_one(&self.pgpool)
        .await?;

        let max: Option<i64> = res.try_get("max")?;
        let sum: Option<BigDecimal> = res.try_get("sum")?;
        let count: Option<i64> = res.try_get("count")?;

        ensure!(
            sum.is_none() == max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: max.unwrap_or(0).try_into()?,
            value: sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
            counter: count
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

        let updated_rows = sqlx::query(
            r#"
                UPDATE tap_horizon_ravs
                    SET last = true
                WHERE 
                    collection_id = $1
                    AND payer = $2
                    AND service_provider = $3
                    AND data_service = $4
            "#,
        )
        // self.allocation_id is already a CollectionId in Horizon state
        .bind(self.allocation_id.encode_hex())
        .bind(self.sender.encode_hex())
        .bind(self.indexer_address.encode_hex())
        .bind(
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futures::future::join_all;
    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use ractor::{call, ActorStatus};
    use serde_json::json;
    use tap_aggregator::grpc::v2::tap_aggregator_client::TapAggregatorClient;
    use test_assets::{flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
    use thegraph_core::{alloy::primitives::U256, CollectionId};
    use tokio::sync::watch;
    use wiremock::{matchers::body_string_contains, Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::tap::CheckingReceipt;
    use crate::test::actors::create_mock_sender_account;
    use crate::test::{
        create_rav_v2, create_received_receipt_v2, get_grpc_url, store_rav_v2, store_receipt,
        ALLOCATION_ID_0, ESCROW_VALUE, TAP_EIP712_DOMAIN_SEPARATOR_V2,
    };

    async fn setup_network_subgraph(redeemed: bool) -> MockServer {
        let mock_server = MockServer::start().await;
        let response = if redeemed {
            json!({
                "data": {
                    "paymentsEscrowTransactions": [
                        { "id": "0x01", "allocationId": ALLOCATION_ID_0.encode_hex(), "timestamp": "1" }
                    ]
                }
            })
        } else {
            json!({ "data": { "paymentsEscrowTransactions": [] } })
        };

        mock_server
            .register(
                Mock::given(body_string_contains("paymentsEscrowTransactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(response)),
            )
            .await;

        mock_server
    }

    async fn build_sender_allocation_args(
        pgpool: PgPool,
        network_subgraph: &'static SubgraphClient,
        sender_account_ref: ActorRef<SenderAccountMessage>,
    ) -> SenderAllocationArgs<Horizon> {
        let (escrow_accounts_tx, escrow_accounts_rx) = watch::channel(EscrowAccounts::default());
        escrow_accounts_tx
            .send(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            ))
            .unwrap();

        let aggregator_url = get_grpc_url().await;
        let sender_aggregator = TapAggregatorClient::connect(aggregator_url)
            .await
            .expect("Failed to connect to test aggregator");

        SenderAllocationArgs::builder()
            .pgpool(pgpool)
            .allocation_id(CollectionId::from(ALLOCATION_ID_0))
            .sender(SENDER.1)
            .escrow_accounts(escrow_accounts_rx)
            .network_subgraph(network_subgraph)
            .domain_separator(TAP_EIP712_DOMAIN_SEPARATOR_V2.clone())
            .sender_account_ref(sender_account_ref)
            .sender_aggregator(sender_aggregator)
            .config(AllocationConfig::from_sender_config(
                crate::test::get_sender_account_config(),
            ))
            .build()
    }

    async fn build_state(
        pgpool: PgPool,
        network_subgraph: &'static SubgraphClient,
    ) -> SenderAllocationState<Horizon> {
        let (_receiver, sender_account_ref) = create_mock_sender_account().await;
        let args = build_sender_allocation_args(pgpool, network_subgraph, sender_account_ref).await;
        SenderAllocationState::new(args).await.unwrap()
    }

    async fn spawn_sender_allocation(
        pgpool: PgPool,
        network_subgraph: &'static SubgraphClient,
    ) -> (
        ActorRef<SenderAllocationMessage>,
        tokio::sync::mpsc::Receiver<SenderAccountMessage>,
    ) {
        let (mut receiver, sender_account_ref) = create_mock_sender_account().await;
        let args = build_sender_allocation_args(pgpool, network_subgraph, sender_account_ref).await;
        let (sender_allocation, _) =
            SenderAllocation::<Horizon>::spawn(None, SenderAllocation::default(), args)
                .await
                .unwrap();
        flush_messages(&mut receiver).await;
        (sender_allocation, receiver)
    }

    #[tokio::test]
    async fn test_several_receipts_rav_request() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        let (sender_allocation, mut receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        for i in 0..AMOUNT_OF_RECEIPTS {
            let receipt =
                create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        let _ = receiver.recv().await;
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }

    #[tokio::test]
    async fn test_several_receipts_batch_insert_rav_request() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        let (sender_allocation, mut receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        for i in 0..AMOUNT_OF_RECEIPTS {
            let receipt =
                create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        let _ = receiver.recv().await;
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }

    #[tokio::test]
    async fn test_close_allocation_no_pending_fees() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        let (sender_allocation, _receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        let res = sqlx::query("SELECT COUNT(*) AS count FROM tap_horizon_ravs")
            .fetch_one(&test_db.pool)
            .await
            .unwrap();
        let count: i64 = res.try_get("count").unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_close_allocation_with_pending_fees() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        for i in 0..10 {
            let receipt =
                create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        let (sender_allocation, _receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        let res = sqlx::query("SELECT COUNT(*) AS count FROM tap_horizon_ravs WHERE last = true")
            .fetch_one(&test_db.pool)
            .await
            .unwrap();
        let count: i64 = res.try_get("count").unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn should_return_unaggregated_fees_without_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));
        let state = build_state(test_db.pool.clone(), network_subgraph).await;

        for i in 1..10 {
            let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();
        assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[tokio::test]
    async fn should_calculate_invalid_receipts_fee() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));
        let mut state = build_state(test_db.pool.clone(), network_subgraph).await;

        struct FailingCheck;
        #[async_trait::async_trait]
        impl Check<TapReceipt> for FailingCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                _receipt: &CheckingReceipt,
            ) -> tap_core::receipt::checks::CheckResult {
                Err(tap_core::receipt::checks::CheckError::Failed(anyhow!(
                    "Failing check"
                )))
            }
        }

        let checks = CheckList::new(vec![Arc::new(FailingCheck)]);
        let checking_receipts = vec![
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
        ];
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
        let failing_receipts = join_all(failing_receipts).await;

        state
            .store_invalid_receipts(failing_receipts)
            .await
            .unwrap();
        let total_invalid_receipts = state.calculate_invalid_receipts_fee().await.unwrap();
        assert_eq!(total_invalid_receipts.value, 3u128);
    }

    #[tokio::test]
    async fn should_return_unaggregated_fees_with_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        let signed_rav = create_rav_v2(
            *CollectionId::from(ALLOCATION_ID_0),
            SIGNER.0.clone(),
            4,
            10,
        );
        store_rav_v2(&test_db.pool, signed_rav, SENDER.1)
            .await
            .unwrap();

        let state = build_state(test_db.pool.clone(), network_subgraph).await;

        for i in 1..10 {
            let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();
        assert_eq!(total_unaggregated_fees.value, 35u128);
    }

    #[tokio::test]
    async fn test_store_failed_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));
        let state = build_state(test_db.pool.clone(), network_subgraph).await;

        let signed_rav = create_rav_v2(
            *CollectionId::from(ALLOCATION_ID_0),
            SIGNER.0.clone(),
            4,
            10,
        );
        state
            .store_failed_rav(&signed_rav.message, &signed_rav, "test")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_store_invalid_receipts() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));
        let mut state = build_state(test_db.pool.clone(), network_subgraph).await;

        struct FailingCheck;
        #[async_trait::async_trait]
        impl Check<TapReceipt> for FailingCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                _receipt: &CheckingReceipt,
            ) -> tap_core::receipt::checks::CheckResult {
                Err(tap_core::receipt::checks::CheckError::Failed(anyhow!(
                    "Failing check"
                )))
            }
        }

        let checks = CheckList::new(vec![Arc::new(FailingCheck)]);
        let checking_receipts = vec![
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
        ];
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
        let failing_receipts = join_all(failing_receipts).await;

        let result = state.store_invalid_receipts(failing_receipts).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mark_rav_last() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));
        let state = build_state(test_db.pool.clone(), network_subgraph).await;

        let signed_rav = create_rav_v2(
            *CollectionId::from(ALLOCATION_ID_0),
            SIGNER.0.clone(),
            4,
            10,
        );
        store_rav_v2(&test_db.pool, signed_rav, SENDER.1)
            .await
            .unwrap();

        let result = state.mark_rav_last().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_failed_rav_request() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(false).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        // Use receipts signed by a wallet not in escrow_accounts to force invalid receipts.
        for i in 0..10 {
            let receipt = create_received_receipt_v2(
                &ALLOCATION_ID_0,
                &crate::test::SENDER_2.0,
                i,
                i + 1,
                i.into(),
            );
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        let (sender_allocation, mut receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        let _ = receiver.recv().await;
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);
    }

    #[tokio::test]
    async fn test_rav_request_when_all_receipts_invalid() {
        let test_db = test_assets::setup_shared_test_db().await;
        let network_mock = setup_network_subgraph(true).await;
        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&network_mock.uri()).unwrap(),
            )
            .await,
        ));

        let timestamp = 1u64;
        const RECEIPT_VALUE: u128 = 10;
        const TOTAL_RECEIPTS: u64 = 10;
        for i in 0..TOTAL_RECEIPTS {
            let receipt = create_received_receipt_v2(
                &ALLOCATION_ID_0,
                &SIGNER.0,
                i,
                timestamp,
                RECEIPT_VALUE,
            );
            let signed = receipt.signed_receipt().0.clone();
            store_receipt(&test_db.pool, &signed).await.unwrap();
        }

        let (sender_allocation, mut receiver) =
            spawn_sender_allocation(test_db.pool.clone(), network_subgraph).await;

        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        let _ = receiver.recv().await;

        let invalid_receipts =
            sqlx::query("SELECT COUNT(*) AS count FROM tap_horizon_receipts_invalid")
                .fetch_one(&test_db.pool)
                .await
                .unwrap();
        let invalid_count: i64 = invalid_receipts.try_get("count").unwrap();
        assert_eq!(invalid_count, 10);

        let remaining = sqlx::query("SELECT COUNT(*) AS count FROM tap_horizon_receipts")
            .fetch_one(&test_db.pool)
            .await
            .unwrap();
        let remaining_count: i64 = remaining.try_get("count").unwrap();
        assert_eq!(remaining_count, 0);
    }
}
