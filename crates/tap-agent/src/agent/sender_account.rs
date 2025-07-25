// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#[allow(unused_imports)] // str::FromStr used in production code paths
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::LazyLock,
    time::Duration,
};

#[allow(unused_imports)] // Used in different conditional compilation modes
use anyhow::Context;
#[allow(unused_imports)] // Used in different conditional compilation modes
use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
#[allow(unused_imports)] // Used in different conditional compilation modes
use futures::stream;
#[allow(unused_imports)] // Used for stream operations
use futures::StreamExt;
#[allow(unused_imports)] // Used in production config structs
use indexer_monitor::{EscrowAccounts, SubgraphClient};
#[allow(unused_imports)] // Used in different conditional compilation modes
use indexer_query::{
    closed_allocations::{self, ClosedAllocations},
    unfinalized_transactions::{self},
    UnfinalizedTransactions,
};
#[allow(unused_imports)] // Used in different conditional compilation modes
use indexer_watcher::watch_pipe;
use prometheus::{register_gauge_vec, register_int_gauge_vec, GaugeVec, IntGaugeVec};
#[cfg(any(test, feature = "test"))]
use ractor::{Actor, ActorProcessingErr, ActorRef, MessagingErr, SupervisionEvent};
#[allow(unused_imports)] // Used in production config structs
use reqwest::Url;
#[allow(unused_imports)] // Used in production config structs
use sqlx::PgPool;
#[allow(unused_imports)] // Used in different conditional compilation modes
use tap_aggregator::grpc::{
    v1::tap_aggregator_client::TapAggregatorClient as AggregatorV1,
    v2::tap_aggregator_client::TapAggregatorClient as AggregatorV2,
};
#[allow(unused_imports)] // hex::ToHexExt and sol_types::Eip712Domain used in production
use thegraph_core::{
    alloy::{
        hex::ToHexExt,
        primitives::{Address, U256},
        sol_types::Eip712Domain,
    },
    AllocationId as AllocationIdCore, CollectionId,
};
#[allow(unused_imports)] // Used in production config structs
use tokio::{sync::watch::Receiver, task::JoinHandle};
#[allow(unused_imports)] // Used in different conditional compilation modes
use tonic::transport::{Channel, Endpoint};
#[allow(unused_imports)] // Used in different conditional compilation modes
use tracing::Level;

#[allow(unused_imports)] // SenderType used for Legacy/Horizon differentiation in production
use super::sender_accounts_manager::{AllocationId, SenderType};

#[cfg(any(test, feature = "test"))]
use super::sender_allocation::{
    AllocationConfig, SenderAllocation, SenderAllocationArgs, SenderAllocationMessage,
};
#[allow(unused_imports)]
// Production-only imports for rate limiting, backoff, network types, tracking
use crate::{
    adaptative_concurrency::AdaptiveLimiter,
    agent::unaggregated_receipts::UnaggregatedReceipts,
    backoff::BackoffInfo,
    tap::context::{Horizon, Legacy},
    tracker::{SenderFeeTracker, SimpleFeeTracker},
};

#[allow(dead_code)] // Prometheus metrics used in production, not in test builds
static SENDER_DENIED: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("tap_sender_denied", "Sender is denied", &["sender"]).unwrap()
});
#[allow(dead_code)] // Used in production code
static ESCROW_BALANCE: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_sender_escrow_balance_grt_total",
        "Sender escrow balance",
        &["sender"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static UNAGGREGATED_FEES: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_unaggregated_fees_grt_total",
        "Unggregated Fees value",
        &["sender", "allocation"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static SENDER_FEE_TRACKER: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_sender_fee_tracker_grt_total",
        "Sender fee tracker metric",
        &["sender"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static INVALID_RECEIPT_FEES: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_invalid_receipt_fees_grt_total",
        "Failed receipt fees",
        &["sender", "allocation"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static PENDING_RAV: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_pending_rav_grt_total",
        "Pending ravs values",
        &["sender", "allocation"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static MAX_FEE_PER_SENDER: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_max_fee_per_sender_grt_total",
        "Max fee per sender in the config",
        &["sender"]
    )
    .unwrap()
});
#[allow(dead_code)] // Used in production code
static RAV_REQUEST_TRIGGER_VALUE: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "tap_rav_request_trigger_value",
        "RAV request trigger value divisor",
        &["sender"]
    )
    .unwrap()
});

#[allow(dead_code)] // Used by AdaptiveLimiter in production code paths
const INITIAL_RAV_REQUEST_CONCURRENT: usize = 1;

type RavMap = HashMap<Address, u128>;
type Balance = U256;

/// Information for Ravs that are abstracted away from the SignedRav itself
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct RavInformation {
    /// Allocation Id of a Rav
    pub allocation_id: Address,
    /// Value Aggregate of a Rav
    pub value_aggregate: u128,
}

impl From<&tap_graph::SignedRav> for RavInformation {
    fn from(value: &tap_graph::SignedRav) -> Self {
        RavInformation {
            allocation_id: value.message.allocationId,
            value_aggregate: value.message.valueAggregate,
        }
    }
}

impl From<tap_graph::SignedRav> for RavInformation {
    fn from(value: tap_graph::SignedRav) -> Self {
        RavInformation {
            allocation_id: value.message.allocationId,
            value_aggregate: value.message.valueAggregate,
        }
    }
}

impl From<&tap_graph::v2::SignedRav> for RavInformation {
    fn from(value: &tap_graph::v2::SignedRav) -> Self {
        RavInformation {
            allocation_id: AllocationIdCore::from(CollectionId::from(value.message.collectionId))
                .into_inner(),
            value_aggregate: value.message.valueAggregate,
        }
    }
}

/// Custom update receipt fee message
///
/// It has different logic depending on the variant
#[derive(Debug)]
#[cfg_attr(any(test, feature = "test"), derive(educe::Educe))]
#[cfg_attr(any(test, feature = "test"), educe(PartialEq, Eq))]
pub enum ReceiptFees {
    /// Adds the receipt value to the fee tracker
    ///
    /// Used when a receipt is received
    NewReceipt(u128, u64),
    /// Overwrite the current fee tracker with the given value
    ///
    /// Used while starting up to signalize the sender it's current value
    UpdateValue(UnaggregatedReceipts),
    /// Overwrite the current fee tracker with the given value
    ///
    /// If the rav response was successful, update the rav tracker
    /// If not, signalize the fee_tracker to apply proper backoff
    RavRequestResponse(
        UnaggregatedReceipts,
        #[cfg_attr(any(test, feature = "test"), educe(PartialEq(ignore)))]
        anyhow::Result<Option<RavInformation>>,
    ),
    /// Ignores all logic and simply retry Allow/Deny and Rav Request logic
    ///
    /// This is used inside a scheduler to trigger a Rav request in case the
    /// sender is denied since the only way to trigger a Rav request is by
    /// receiving a receipt and denied senders don't receive receipts
    Retry,
}

impl Clone for ReceiptFees {
    fn clone(&self) -> Self {
        match self {
            ReceiptFees::NewReceipt(value, timestamp) => {
                ReceiptFees::NewReceipt(*value, *timestamp)
            }
            ReceiptFees::UpdateValue(receipts) => ReceiptFees::UpdateValue(*receipts),
            ReceiptFees::RavRequestResponse(receipts, result) => {
                // For the Result<Option<RavInformation>>, we need to handle it carefully
                // since anyhow::Error doesn't implement Clone
                let cloned_result = match result {
                    Ok(val) => Ok(val.clone()),
                    Err(_) => Err(anyhow::anyhow!("Cloned error from original anyhow result")),
                };
                ReceiptFees::RavRequestResponse(*receipts, cloned_result)
            }
            ReceiptFees::Retry => ReceiptFees::Retry,
        }
    }
}

/// Enum containing all types of messages that a [SenderAccount] can receive
#[derive(Debug)]
pub enum SenderAccountMessage {
    /// Updates the sender balance and
    UpdateBalanceAndLastRavs(Balance, RavMap),
    /// Spawn and Stop SenderAllocations that were added or removed
    /// in comparision with it current state and updates the state
    UpdateAllocationIds(HashSet<AllocationId>),
    /// Manual request to create a new Sender Allocation
    NewAllocationId(AllocationId),
    /// Updates the fee tracker for a given allocation
    ///
    /// All allowing or denying logic is called inside the message handler
    /// as well as requesting the underlaying allocation rav request
    ///
    /// Custom behavior is defined in [ReceiptFees]
    UpdateReceiptFees(AllocationId, ReceiptFees),
    /// Updates the counter for invalid receipts and verify to deny sender
    UpdateInvalidReceiptFees(AllocationId, UnaggregatedReceipts),
    /// Update rav tracker
    UpdateRav(RavInformation),
    #[cfg(test)]
    /// Returns the sender fee tracker, used for tests
    GetSenderFeeTracker(ractor::RpcReplyPort<SenderFeeTracker>),
    #[cfg(test)]
    /// Returns the Deny status, used for tests
    GetDeny(ractor::RpcReplyPort<bool>),
    #[cfg(test)]
    /// Returns if the scheduler is enabled, used for tests
    IsSchedulerEnabled(ractor::RpcReplyPort<bool>),
}

// Manual Clone implementation to handle RpcReplyPort fields properly
impl Clone for SenderAccountMessage {
    fn clone(&self) -> Self {
        match self {
            SenderAccountMessage::UpdateBalanceAndLastRavs(balance, rav_map) => {
                SenderAccountMessage::UpdateBalanceAndLastRavs(*balance, rav_map.clone())
            }
            SenderAccountMessage::UpdateAllocationIds(ids) => {
                SenderAccountMessage::UpdateAllocationIds(ids.clone())
            }
            SenderAccountMessage::NewAllocationId(id) => SenderAccountMessage::NewAllocationId(*id),
            SenderAccountMessage::UpdateReceiptFees(id, fees) => {
                SenderAccountMessage::UpdateReceiptFees(*id, fees.clone())
            }
            SenderAccountMessage::UpdateInvalidReceiptFees(id, fees) => {
                SenderAccountMessage::UpdateInvalidReceiptFees(*id, *fees)
            }
            SenderAccountMessage::UpdateRav(rav) => SenderAccountMessage::UpdateRav(rav.clone()),
            #[cfg(test)]
            SenderAccountMessage::GetSenderFeeTracker(_reply_port) => {
                // For tests, create a dummy reply port - this is needed for message forwarding but
                // the actual reply port can't be meaningfully cloned
                let (tx, _rx) = tokio::sync::oneshot::channel();
                SenderAccountMessage::GetSenderFeeTracker(ractor::RpcReplyPort::from(tx))
            }
            #[cfg(test)]
            SenderAccountMessage::GetDeny(_reply_port) => {
                // For tests, create a dummy reply port
                let (tx, _rx) = tokio::sync::oneshot::channel();
                SenderAccountMessage::GetDeny(ractor::RpcReplyPort::from(tx))
            }
            #[cfg(test)]
            SenderAccountMessage::IsSchedulerEnabled(_reply_port) => {
                // For tests, create a dummy reply port
                let (tx, _rx) = tokio::sync::oneshot::channel();
                SenderAccountMessage::IsSchedulerEnabled(ractor::RpcReplyPort::from(tx))
            }
        }
    }
}

#[cfg(any(test, feature = "test"))]
impl PartialEq for SenderAccountMessage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                SenderAccountMessage::UpdateBalanceAndLastRavs(balance1, rav_map1),
                SenderAccountMessage::UpdateBalanceAndLastRavs(balance2, rav_map2),
            ) => balance1 == balance2 && rav_map1 == rav_map2,
            (
                SenderAccountMessage::UpdateAllocationIds(ids1),
                SenderAccountMessage::UpdateAllocationIds(ids2),
            ) => ids1 == ids2,
            (
                SenderAccountMessage::NewAllocationId(id1),
                SenderAccountMessage::NewAllocationId(id2),
            ) => id1 == id2,
            (
                SenderAccountMessage::UpdateReceiptFees(id1, fees1),
                SenderAccountMessage::UpdateReceiptFees(id2, fees2),
            ) => id1 == id2 && fees1 == fees2,
            (
                SenderAccountMessage::UpdateInvalidReceiptFees(id1, fees1),
                SenderAccountMessage::UpdateInvalidReceiptFees(id2, fees2),
            ) => id1 == id2 && fees1 == fees2,
            (SenderAccountMessage::UpdateRav(rav1), SenderAccountMessage::UpdateRav(rav2)) => {
                rav1 == rav2
            }
            #[cfg(test)]
            // For RpcReplyPort messages, we ignore the reply port in comparison (like educe did)
            (
                SenderAccountMessage::GetSenderFeeTracker(_),
                SenderAccountMessage::GetSenderFeeTracker(_),
            ) => true,
            #[cfg(test)]
            (SenderAccountMessage::GetDeny(_), SenderAccountMessage::GetDeny(_)) => true,
            #[cfg(test)]
            (
                SenderAccountMessage::IsSchedulerEnabled(_),
                SenderAccountMessage::IsSchedulerEnabled(_),
            ) => true,
            _ => false,
        }
    }
}

#[cfg(any(test, feature = "test"))]
impl Eq for SenderAccountMessage {}

#[cfg(any(test, feature = "test"))]
/// A SenderAccount manages the receipts accounting between the indexer and the sender across
/// multiple allocations.
///
/// Manages the lifecycle of TAP for the SenderAccount, including:
/// - Monitoring new receipts and keeping track of the cumulative unaggregated fees across
///   allocations.
/// - Requesting RAVs from the sender's TAP aggregator once the cumulative unaggregated fees reach a
///   certain threshold.
/// - Requesting the last RAV from the sender's TAP aggregator for all EOL allocations.
pub struct SenderAccount;

#[cfg(any(test, feature = "test"))]
/// Arguments received in startup while spawing [SenderAccount] actor
pub struct SenderAccountArgs {
    /// Configuration derived from config.toml
    pub config: &'static SenderAccountConfig,

    /// Connection to database
    pub pgpool: PgPool,
    /// Current sender address
    pub sender_id: Address,
    /// Watcher that returns a list of escrow accounts for current indexer
    pub escrow_accounts: Receiver<EscrowAccounts>,
    /// Watcher that returns a set of open and recently closed allocation ids
    pub indexer_allocations: Receiver<HashSet<AllocationId>>,
    /// SubgraphClient of the escrow subgraph
    pub escrow_subgraph: &'static SubgraphClient,
    /// SubgraphClient of the network subgraph
    pub network_subgraph: &'static SubgraphClient,
    /// Domain separator used for tap
    pub domain_separator: Eip712Domain,
    /// Endpoint URL for aggregator server
    pub sender_aggregator_endpoint: Url,
    /// List of allocation ids that must created at startup
    pub allocation_ids: HashSet<AllocationId>,
    /// Prefix used to bypass limitations of global actor registry (used for tests)
    pub prefix: Option<String>,

    /// Configuration for retry scheduler in case sender is denied
    pub retry_interval: Duration,

    /// Sender type, used to decide which set of tables to use
    pub sender_type: SenderType,
}

#[cfg(any(test, feature = "test"))]
/// State used by the actor
///
/// This is a separate instance that makes it easier to have mutable
/// reference, for more information check ractor library
pub struct State {
    /// Prefix used to bypass limitations of global actor registry (used for tests)
    prefix: Option<String>,
    /// Tracker used to monitor all pending fees across allocations
    ///
    /// Since rav requests are per allocation, this also has the algorithm
    /// to select the next allocation to have a rav request.
    ///
    /// This monitors if rav requests succeeds or fails and apply proper backoff.
    ///
    /// Keeps track of the buffer returning values for both inside or outside the buffer.
    ///
    /// It selects the allocation with most amount of pending fees.
    /// Filters out allocations in the algorithm in case:
    ///     - In back-off
    ///     - Marked as closing allocation (blocked)
    ///     - Rav request in flight (selected the previous time)
    sender_fee_tracker: SenderFeeTracker,
    /// Simple tracker used to monitor all Ravs that were not redeemed yet.
    ///
    /// This is used to monitor both active allocations and closed but not redeemed.
    rav_tracker: SimpleFeeTracker,
    /// Simple tracker used to monitor all invalid receipts ever.
    invalid_receipts_tracker: SimpleFeeTracker,
    /// Set containing current active allocations
    allocation_ids: HashSet<AllocationId>,
    /// Scheduler used to send a retry message in case sender is denied
    ///
    /// If scheduler is set, it's canceled in the first [SenderAccountMessage::UpdateReceiptFees]
    /// message
    scheduled_rav_request: Option<JoinHandle<Result<(), MessagingErr<SenderAccountMessage>>>>,

    /// Current sender address
    sender: Address,

    /// State to check if sender is current denied
    denied: bool,
    /// Sender Balance used to verify if it has money in
    /// the escrow to pay for all non-redeemed fees (ravs and receipts)
    sender_balance: U256,
    /// Configuration for retry scheduler in case sender is denied
    retry_interval: Duration,

    /// Adaptative limiter for concurrent Rav Request
    ///
    /// This uses a simple algorithm where it increases by one in case
    /// of a success or decreases by half in case of a failure
    adaptive_limiter: AdaptiveLimiter,

    /// Watcher containing the escrow accounts
    escrow_accounts: Receiver<EscrowAccounts>,

    /// SubgraphClient of the escrow subgraph
    escrow_subgraph: &'static SubgraphClient,
    /// SubgraphClient of the network subgraph
    network_subgraph: &'static SubgraphClient,

    /// Domain separator used for tap
    domain_separator: Eip712Domain,
    /// Database connection
    pgpool: PgPool,
    /// Aggregator client for V1
    ///
    /// This is only send to [SenderAllocation] in case
    /// it's a [AllocationId::Legacy]
    aggregator_v1: AggregatorV1<Channel>,
    /// Aggregator client for V2
    ///
    /// This is only send to [SenderAllocation] in case
    /// it's a [AllocationId::Horizon]
    aggregator_v2: AggregatorV2<Channel>,

    // Used as a global backoff for triggering new rav requests
    //
    // This is used when there are failures in Rav request and
    // reset in case of a successful response
    backoff_info: BackoffInfo,

    /// Allows the sender to go over escrow balance
    /// limited to `max_amount_willing_to_lose_grt`
    trusted_sender: bool,

    /// Sender type, used to decide which set of tables to use
    sender_type: SenderType,

    // Config forwarded to [SenderAllocation]
    config: &'static SenderAccountConfig,
}

/// Configuration derived from config.toml
pub struct SenderAccountConfig {
    /// Buffer used for the receipts
    pub rav_request_buffer: Duration,
    /// Maximum amount is willing to lose
    pub max_amount_willing_to_lose_grt: u128,
    /// What value triggers a new Rav request
    pub trigger_value: u128,

    // allocation config
    /// Timeout config for rav requests
    pub rav_request_timeout: Duration,
    /// Limit of receipts sent in a Rav Request
    pub rav_request_receipt_limit: u64,
    /// Current indexer address
    pub indexer_address: Address,
    /// Polling interval for escrow subgraph
    pub escrow_polling_interval: Duration,
    /// Timeout used while creating [SenderAccount]
    ///
    /// This is reached if the database is too slow
    pub tap_sender_timeout: Duration,
    /// Senders that are allowed to spend up to `max_amount_willing_to_lose_grt`
    /// over the escrow balance
    pub trusted_senders: HashSet<Address>,

    #[doc(hidden)]
    pub horizon_enabled: bool,
}

impl SenderAccountConfig {
    /// Creates a [SenderAccountConfig] by getting a reference of [indexer_config::Config]
    pub fn from_config(config: &indexer_config::Config) -> Self {
        Self {
            rav_request_buffer: config.tap.rav_request.timestamp_buffer_secs,
            rav_request_receipt_limit: config.tap.rav_request.max_receipts_per_request,
            indexer_address: config.indexer.indexer_address,
            escrow_polling_interval: config.subgraphs.escrow.config.syncing_interval_secs,
            max_amount_willing_to_lose_grt: config.tap.max_amount_willing_to_lose_grt.get_value(),
            trigger_value: config.tap.get_trigger_value(),
            rav_request_timeout: config.tap.rav_request.request_timeout_secs,
            tap_sender_timeout: config.tap.sender_timeout_secs,
            trusted_senders: config.tap.trusted_senders.clone(),
            horizon_enabled: config.horizon.enabled,
        }
    }
}

#[cfg(any(test, feature = "test"))]
impl State {
    /// Spawn a sender allocation given the allocation_id
    ///
    /// Since this is a function inside State, we need to provide
    /// the reference for the [SenderAccount] actor
    async fn create_sender_allocation(
        &self,
        sender_account_ref: ActorRef<SenderAccountMessage>,
        allocation_id: AllocationId,
    ) -> anyhow::Result<()> {
        tracing::trace!(
            %self.sender,
            %allocation_id,
            "SenderAccount is creating allocation."
        );

        // Check if actor already exists to prevent race condition during concurrent creation attempts
        let actor_name = self.format_sender_allocation(&allocation_id.address());
        if ActorRef::<SenderAllocationMessage>::where_is(actor_name.clone()).is_some() {
            tracing::debug!(
                %self.sender,
                %allocation_id,
                actor_name = %actor_name,
                "SenderAllocation actor already exists, skipping creation"
            );
            return Ok(());
        }

        match allocation_id {
            AllocationId::Legacy(id) => {
                let args = SenderAllocationArgs::builder()
                    .pgpool(self.pgpool.clone())
                    .allocation_id(id)
                    .sender(self.sender)
                    .escrow_accounts(self.escrow_accounts.clone())
                    .escrow_subgraph(self.escrow_subgraph)
                    .domain_separator(self.domain_separator.clone())
                    .sender_account_ref(sender_account_ref.clone())
                    .sender_aggregator(self.aggregator_v1.clone())
                    .config(AllocationConfig::from_sender_config(self.config))
                    .build();
                SenderAllocation::<Legacy>::spawn_linked(
                    Some(self.format_sender_allocation(&id)),
                    SenderAllocation::default(),
                    args,
                    sender_account_ref.get_cell(),
                )
                .await?;
            }
            AllocationId::Horizon(id) => {
                let args = SenderAllocationArgs::builder()
                    .pgpool(self.pgpool.clone())
                    .allocation_id(id)
                    .sender(self.sender)
                    .escrow_accounts(self.escrow_accounts.clone())
                    .escrow_subgraph(self.escrow_subgraph)
                    .domain_separator(self.domain_separator.clone())
                    .sender_account_ref(sender_account_ref.clone())
                    .sender_aggregator(self.aggregator_v2.clone())
                    .config(AllocationConfig::from_sender_config(self.config))
                    .build();

                SenderAllocation::<Horizon>::spawn_linked(
                    Some(self.format_sender_allocation(&id.as_address())),
                    SenderAllocation::default(),
                    args,
                    sender_account_ref.get_cell(),
                )
                .await?;
            }
        }
        Ok(())
    }
    fn format_sender_allocation(&self, allocation_id: &Address) -> String {
        let mut sender_allocation_id = String::new();
        if let Some(prefix) = &self.prefix {
            sender_allocation_id.push_str(prefix);
            sender_allocation_id.push(':');
        }
        sender_allocation_id.push_str(&format!("{}:{}", self.sender, allocation_id));
        sender_allocation_id
    }

    async fn rav_request_for_heaviest_allocation(&mut self) -> anyhow::Result<()> {
        let allocation_id = self
            .sender_fee_tracker
            .get_heaviest_allocation_id()
            .ok_or_else(|| {
                self.backoff_info.fail();
                anyhow::anyhow!(
                    "Error while getting the heaviest allocation, \
            this is due one of the following reasons: \n
            1. allocations have too much fees under their buffer\n
            2. allocations are blocked to be redeemed due to ongoing last rav. \n
            If you keep seeing this message try to increase your `amount_willing_to_lose` \
            and restart your `tap-agent`\n
            If this doesn't work, open an issue on our Github."
                )
            })?;
        self.backoff_info.ok();
        self.rav_request_for_allocation(allocation_id).await
    }

    async fn rav_request_for_allocation(&mut self, allocation_id: Address) -> anyhow::Result<()> {
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation actor {allocation_id}");
        };

        allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Error while sending and waiting message for actor {allocation_id}. Error: {e}"
                )
            })?;
        self.adaptive_limiter.acquire();
        self.sender_fee_tracker.start_rav_request(allocation_id);

        Ok(())
    }

    /// Proccess the rav response sent by [SenderAllocation]
    ///
    /// This updates all backoff information for fee_tracker, backoff_info and
    /// adaptative_limiter as well as updating the rav tracker and fee tracker
    fn finalize_rav_request(
        &mut self,
        allocation_id: Address,
        rav_response: (UnaggregatedReceipts, anyhow::Result<Option<RavInformation>>),
    ) {
        self.sender_fee_tracker.finish_rav_request(allocation_id);
        let (fees, rav_result) = rav_response;
        match rav_result {
            Ok(signed_rav) => {
                self.sender_fee_tracker.ok_rav_request(allocation_id);
                self.adaptive_limiter.on_success();
                let rav_value = signed_rav.map_or(0, |rav| rav.value_aggregate);
                self.update_rav(allocation_id, rav_value);
            }
            Err(err) => {
                self.sender_fee_tracker.failed_rav_backoff(allocation_id);
                self.adaptive_limiter.on_failure();
                tracing::error!(
                    "Error while requesting RAV for sender {} and allocation {}: {}",
                    self.sender,
                    allocation_id,
                    err
                );
            }
        };
        self.update_sender_fee(allocation_id, fees);
    }

    fn update_rav(&mut self, allocation_id: Address, rav_value: u128) {
        self.rav_tracker.update(allocation_id, rav_value);
        PENDING_RAV
            .with_label_values(&[&self.sender.to_string(), &allocation_id.to_string()])
            .set(rav_value as f64);
    }

    fn update_sender_fee(
        &mut self,
        allocation_id: Address,
        unaggregated_fees: UnaggregatedReceipts,
    ) {
        self.sender_fee_tracker
            .update(allocation_id, unaggregated_fees);
        SENDER_FEE_TRACKER
            .with_label_values(&[&self.sender.to_string()])
            .set(self.sender_fee_tracker.get_total_fee() as f64);

        UNAGGREGATED_FEES
            .with_label_values(&[&self.sender.to_string(), &allocation_id.to_string()])
            .set(unaggregated_fees.value as f64);
    }

    /// Determines whether the sender should be denied/blocked based on current fees and balance.
    ///
    /// The deny condition is reached when either:
    /// 1. Total potential fees (pending RAVs + unaggregated fees) exceed the sender's balance
    /// 2. Total risky fees (unaggregated + invalid) exceed max_amount_willing_to_lose
    ///
    /// When a successful RAV request clears unaggregated fees, this function should return
    /// false, indicating the deny condition is resolved and retries can stop.
    ///
    /// This is the core logic that determines when the retry mechanism should continue
    /// versus when it should stop after successful RAV processing.
    fn deny_condition_reached(&self) -> bool {
        let pending_ravs = self.rav_tracker.get_total_fee();
        let unaggregated_fees = self.sender_fee_tracker.get_total_fee();
        let max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt;

        // if it's a trusted sender, allow to spend up to max_amount_willing_to_lose
        let balance = if self.trusted_sender {
            self.sender_balance + U256::from(max_amount_willing_to_lose)
        } else {
            self.sender_balance
        };

        let pending_fees_over_balance = U256::from(pending_ravs + unaggregated_fees) >= balance;
        let invalid_receipt_fees = self.invalid_receipts_tracker.get_total_fee();
        let total_fee_over_max_value =
            unaggregated_fees + invalid_receipt_fees >= max_amount_willing_to_lose;

        tracing::trace!(
            trusted_sender = %self.trusted_sender,
            %pending_fees_over_balance,
            %total_fee_over_max_value,
            "Verifying if deny condition was reached.",
        );

        total_fee_over_max_value || pending_fees_over_balance
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    async fn add_to_denylist(&mut self) {
        tracing::warn!(
            trusted_sender = %self.trusted_sender,
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt,
            sender_balance = self.sender_balance.to_u128(),
            "Denying sender."
        );
        SenderAccount::deny_sender(self.sender_type, &self.pgpool, self.sender).await;
        self.denied = true;
        SENDER_DENIED
            .with_label_values(&[&self.sender.to_string()])
            .set(1);
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    async fn remove_from_denylist(&mut self) {
        tracing::info!(
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt,
            sender_balance = self.sender_balance.to_u128(),
            "Allowing sender."
        );
        match self.sender_type {
            SenderType::Legacy => {
                sqlx::query!(
                    r#"
                    DELETE FROM scalar_tap_denylist
                    WHERE sender_address = $1
                "#,
                    self.sender.encode_hex(),
                )
                .execute(&self.pgpool)
                .await
                .expect("Should not fail to delete from denylist");
            }
            SenderType::Horizon => {
                if self.config.horizon_enabled {
                    sqlx::query!(
                        r#"
                    DELETE FROM tap_horizon_denylist
                    WHERE sender_address = $1
                "#,
                        self.sender.encode_hex(),
                    )
                    .execute(&self.pgpool)
                    .await
                    .expect("Should not fail to delete from horizon denylist");
                }
            }
        }
        self.denied = false;

        SENDER_DENIED
            .with_label_values(&[&self.sender.to_string()])
            .set(0);
    }

    /// Receives a list of possible closed allocations and verify
    /// if they are really closed in the subgraph
    async fn check_closed_allocations(
        &self,
        allocation_ids: HashSet<&AllocationId>,
    ) -> anyhow::Result<HashSet<Address>> {
        if allocation_ids.is_empty() {
            return Ok(HashSet::new());
        }
        // We don't need to check what type of allocation it is since
        // legacy allocation ids can't be reused for horizon
        let allocation_ids: Vec<String> = allocation_ids
            .into_iter()
            .map(|addr| addr.to_string().to_lowercase())
            .collect();

        let mut hash: Option<String> = None;
        let mut last: Option<String> = None;
        let mut responses = vec![];
        let page_size = 200;

        loop {
            let result = self
                .network_subgraph
                .query::<ClosedAllocations, _>(closed_allocations::Variables {
                    allocation_ids: allocation_ids.clone(),
                    first: page_size,
                    last: last.unwrap_or_default(),
                    block: hash.map(|hash| closed_allocations::Block_height {
                        hash: Some(hash),
                        number: None,
                        number_gte: None,
                    }),
                })
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            let mut data = result?;
            let page_len = data.allocations.len();

            hash = data.meta.and_then(|meta| meta.block.hash);
            last = data.allocations.last().map(|entry| entry.id.to_string());

            responses.append(&mut data.allocations);
            if (page_len as i64) < page_size {
                break;
            }
        }
        Ok(responses
            .into_iter()
            .map(|allocation| Address::from_str(&allocation.id))
            .collect::<Result<HashSet<_>, _>>()?)
    }
}

#[cfg(any(test, feature = "test"))]
/// Actor implementation for [SenderAccount]
#[async_trait::async_trait]
impl Actor for SenderAccount {
    type Msg = SenderAccountMessage;
    type State = State;
    type Arguments = SenderAccountArgs;

    /// This is called in the [ractor::Actor::spawn] method and is used
    /// to process the [SenderAccountArgs] with a reference to the current
    /// actor
    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        SenderAccountArgs {
            config,
            pgpool,
            sender_id,
            escrow_accounts,
            indexer_allocations,
            escrow_subgraph,
            network_subgraph,
            domain_separator,
            sender_aggregator_endpoint,
            allocation_ids,
            prefix,
            retry_interval,
            sender_type,
        }: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let myself_clone = myself.clone();
        watch_pipe(indexer_allocations, move |allocation_ids| {
            let allocation_ids = allocation_ids.clone();
            // Update the allocation_ids
            myself_clone
                .cast(SenderAccountMessage::UpdateAllocationIds(allocation_ids))
                .unwrap_or_else(|e| {
                    tracing::error!("Error while updating allocation_ids: {:?}", e);
                });
            async {}
        });

        let myself_clone = myself.clone();
        let pgpool_clone = pgpool.clone();
        let accounts_clone = escrow_accounts.clone();
        watch_pipe(accounts_clone, move |escrow_account| {
            let myself = myself_clone.clone();
            let pgpool = pgpool_clone.clone();
            // Get balance or default value for sender
            // this balance already takes into account thawing
            let balance = escrow_account
                .get_balance_for_sender(&sender_id)
                .unwrap_or_default();
            async move {
                let last_non_final_ravs: Vec<_> = match sender_type {
                    // Get all ravs from v1 table
                    SenderType::Legacy => sqlx::query!(
                        r#"
                                    SELECT allocation_id, value_aggregate
                                    FROM scalar_tap_ravs
                                    WHERE sender_address = $1 AND last AND NOT final;
                                "#,
                        sender_id.encode_hex(),
                    )
                    .fetch_all(&pgpool)
                    .await
                    .expect("Should not fail to fetch from scalar_tap_ravs")
                    .into_iter()
                    .map(|record| (record.allocation_id, record.value_aggregate))
                    .collect(),
                    // Get all ravs from v2 table
                    SenderType::Horizon => {
                        if config.horizon_enabled {
                            sqlx::query!(
                                r#"
                                    SELECT collection_id, value_aggregate
                                    FROM tap_horizon_ravs
                                    WHERE payer = $1 AND last AND NOT final;
                                "#,
                                sender_id.encode_hex(),
                            )
                            .fetch_all(&pgpool)
                            .await
                            .expect("Should not fail to fetch from \"horizon\" scalar_tap_ravs")
                            .into_iter()
                            .map(|record| (record.collection_id, record.value_aggregate))
                            .collect()
                        } else {
                            vec![]
                        }
                    }
                };

                // get a list from the subgraph of which subgraphs were already redeemed and were not marked as final
                let redeemed_ravs_allocation_ids = match sender_type {
                    SenderType::Legacy => {
                        // This query returns unfinalized transactions for v1
                        match escrow_subgraph
                            .query::<UnfinalizedTransactions, _>(
                                unfinalized_transactions::Variables {
                                    unfinalized_ravs_allocation_ids: last_non_final_ravs
                                        .iter()
                                        .map(|rav| rav.0.to_string())
                                        .collect::<Vec<_>>(),
                                    sender: format!("{sender_id:x?}"),
                                },
                            )
                            .await
                        {
                            Ok(Ok(response)) => response
                                .transactions
                                .into_iter()
                                .map(|tx| {
                                    tx.allocation_id
                                        .expect("all redeem tx must have allocation_id")
                                })
                                .collect::<Vec<_>>(),
                            // if we have any problems, we don't want to filter out
                            _ => vec![],
                        }
                    }
                    SenderType::Horizon => {
                        if config.horizon_enabled {
                            // V2 doesn't have transaction tracking like V1, but we can check if the RAVs
                            // we're about to redeem are still the latest ones by querying LatestRavs.
                            // If the subgraph has newer RAVs, it means ours were already redeemed.
                            use indexer_query::latest_ravs_v2::{self, LatestRavs};

                            let collection_ids: Vec<String> = last_non_final_ravs
                                .iter()
                                .map(|(collection_id, _)| collection_id.clone())
                                .collect();

                            if !collection_ids.is_empty() {
                                // For V2, use the indexer address as the data service since the indexer
                                // is providing the data service for the queries
                                let data_service = config.indexer_address;

                                match escrow_subgraph
                                    .query::<LatestRavs, _>(latest_ravs_v2::Variables {
                                        payer: format!("{sender_id:x?}"),
                                        data_service: format!("{data_service:x?}"),
                                        service_provider: format!("{:x?}", config.indexer_address),
                                        collection_ids: collection_ids.clone(),
                                    })
                                    .await
                                {
                                    Ok(Ok(response)) => {
                                        // Create a map of our current RAVs for easy lookup
                                        let our_ravs: HashMap<String, u128> = last_non_final_ravs
                                            .iter()
                                            .map(|(collection_id, value)| {
                                                let value_u128 = value
                                                    .to_bigint()
                                                    .and_then(|v| v.to_u128())
                                                    .unwrap_or(0);
                                                (collection_id.clone(), value_u128)
                                            })
                                            .collect();

                                        // Check which RAVs have been updated (indicating redemption)
                                        let mut finalized_allocation_ids = vec![];
                                        for rav in response.latest_ravs {
                                            if let Some(&our_value) = our_ravs.get(&rav.id) {
                                                // If the subgraph RAV has higher value, our RAV was redeemed
                                                if let Ok(subgraph_value) =
                                                    rav.value_aggregate.parse::<u128>()
                                                {
                                                    if subgraph_value > our_value {
                                                        // Return collection ID string for filtering
                                                        finalized_allocation_ids.push(rav.id);
                                                    }
                                                }
                                            }
                                        }
                                        finalized_allocation_ids
                                    }
                                    Ok(Err(e)) => {
                                        tracing::warn!(
                                            error = %e,
                                            sender = %sender_id,
                                            "Failed to query V2 latest RAVs, assuming none are finalized"
                                        );
                                        vec![]
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            sender = %sender_id,
                                            "Failed to execute V2 latest RAVs query, assuming none are finalized"
                                        );
                                        vec![]
                                    }
                                }
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    }
                };

                // filter the ravs marked as last that were not redeemed yet
                let non_redeemed_ravs = last_non_final_ravs
                    .into_iter()
                    .filter_map(|rav| {
                        Some((
                            Address::from_str(&rav.0).ok()?,
                            rav.1.to_bigint().and_then(|v| v.to_u128())?,
                        ))
                    })
                    .filter(|(allocation, _value)| {
                        !redeemed_ravs_allocation_ids.contains(&format!("{allocation:x?}"))
                    })
                    .collect::<HashMap<_, _>>();

                // Update the allocation_ids
                myself
                    .cast(SenderAccountMessage::UpdateBalanceAndLastRavs(
                        balance,
                        non_redeemed_ravs,
                    ))
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "Error while updating balance for sender {}: {:?}",
                            sender_id,
                            e
                        );
                    });
            }
        });

        let denied = match sender_type {
            // Get deny status from the scalar_tap_denylist table
            SenderType::Legacy => sqlx::query!(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM scalar_tap_denylist
                    WHERE sender_address = $1
                ) as denied
            "#,
                sender_id.encode_hex(),
            )
            .fetch_one(&pgpool)
            .await?
            .denied
            .expect("Deny status cannot be null"),
            // Get deny status from the tap horizon table
            SenderType::Horizon => {
                if config.horizon_enabled {
                    sqlx::query!(
                        r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM tap_horizon_denylist
                    WHERE sender_address = $1
                ) as denied
            "#,
                        sender_id.encode_hex(),
                    )
                    .fetch_one(&pgpool)
                    .await?
                    .denied
                    .expect("Deny status cannot be null")
                } else {
                    // If horizon is enabled,
                    // just ignore this sender
                    false
                }
            }
        };

        let sender_balance = escrow_accounts
            .borrow()
            .get_balance_for_sender(&sender_id)
            .unwrap_or_default();

        SENDER_DENIED
            .with_label_values(&[&sender_id.to_string()])
            .set(denied as i64);

        MAX_FEE_PER_SENDER
            .with_label_values(&[&sender_id.to_string()])
            .set(config.max_amount_willing_to_lose_grt as f64);

        RAV_REQUEST_TRIGGER_VALUE
            .with_label_values(&[&sender_id.to_string()])
            .set(config.trigger_value as f64);

        let endpoint = Endpoint::new(sender_aggregator_endpoint.to_string())
            .context("Failed to create an endpoint for the sender aggregator")?;

        let aggregator_v1 = AggregatorV1::connect(endpoint.clone())
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to the TapAggregator endpoint '{}'",
                    endpoint.uri()
                )
            })?;
        // wiremock_grpc used for tests doesn't support Zstd compression
        #[cfg(not(test))]
        let aggregator_v1 = aggregator_v1.send_compressed(tonic::codec::CompressionEncoding::Zstd);

        let aggregator_v2 = AggregatorV2::connect(endpoint.clone())
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to the TapAggregator endpoint '{}'",
                    endpoint.uri()
                )
            })?;
        // wiremock_grpc used for tests doesn't support Zstd compression
        #[cfg(not(test))]
        let aggregator_v2 = aggregator_v2.send_compressed(tonic::codec::CompressionEncoding::Zstd);
        let state = State {
            prefix,
            sender_fee_tracker: SenderFeeTracker::new(config.rav_request_buffer),
            rav_tracker: SimpleFeeTracker::default(),
            invalid_receipts_tracker: SimpleFeeTracker::default(),
            allocation_ids: allocation_ids.clone(),
            scheduled_rav_request: None,
            sender: sender_id,
            denied,
            sender_balance,
            retry_interval,
            adaptive_limiter: AdaptiveLimiter::new(INITIAL_RAV_REQUEST_CONCURRENT, 1..50),
            escrow_accounts,
            escrow_subgraph,
            network_subgraph,
            domain_separator,
            pgpool,
            aggregator_v1,
            aggregator_v2,
            backoff_info: BackoffInfo::default(),
            trusted_sender: config.trusted_senders.contains(&sender_id),
            config,
            sender_type,
        };

        stream::iter(allocation_ids)
            // Create a sender allocation for each allocation
            .map(|allocation_id| state.create_sender_allocation(myself.clone(), allocation_id))
            .buffer_unordered(10) // Limit concurrency to 10 allocations at a time
            .collect::<Vec<anyhow::Result<()>>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<()>>>()?;

        tracing::info!(sender = %sender_id, "SenderAccount created!");
        Ok(state)
    }

    /// Handle a new [SenderAccountMessage] message
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::span!(
            Level::TRACE,
            "SenderAccount handle()",
            sender = %state.sender,
        );
        tracing::trace!(
            message = ?message,
            "New SenderAccount message"
        );

        match message {
            SenderAccountMessage::UpdateRav(RavInformation {
                allocation_id,
                value_aggregate,
            }) => {
                state.update_rav(allocation_id, value_aggregate);

                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }
            }
            SenderAccountMessage::UpdateInvalidReceiptFees(allocation_id, unaggregated_fees) => {
                INVALID_RECEIPT_FEES
                    .with_label_values(&[&state.sender.to_string(), &allocation_id.to_string()])
                    .set(unaggregated_fees.value as f64);

                state
                    .invalid_receipts_tracker
                    .update(allocation_id.address(), unaggregated_fees.value);

                // invalid receipts can't go down
                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }
            }
            SenderAccountMessage::UpdateReceiptFees(allocation_id, receipt_fees) => {
                // If we're here because of a new receipt, abort any scheduled UpdateReceiptFees
                if let Some(scheduled_rav_request) = state.scheduled_rav_request.take() {
                    scheduled_rav_request.abort();
                }

                match receipt_fees {
                    ReceiptFees::NewReceipt(value, timestamp_ns) => {
                        // If state is denied and received new receipt, sender was removed manually from DB
                        if state.denied {
                            tracing::warn!(
                                "
                                No new receipts should have been received, sender has been denied before. \
                                You ***SHOULD NOT*** remove a denied sender manually from the database. \
                                If you do so you are exposing yourself to potentially ****LOSING ALL*** of your query
                                fee ***MONEY***.
                                "
                            );
                            SenderAccount::deny_sender(
                                state.sender_type,
                                &state.pgpool,
                                state.sender,
                            )
                            .await;
                        }

                        // add new value
                        state
                            .sender_fee_tracker
                            .add(allocation_id.address(), value, timestamp_ns);

                        SENDER_FEE_TRACKER
                            .with_label_values(&[&state.sender.to_string()])
                            .set(state.sender_fee_tracker.get_total_fee() as f64);
                        UNAGGREGATED_FEES
                            .with_label_values(&[
                                &state.sender.to_string(),
                                &allocation_id.to_string(),
                            ])
                            .set(
                                state
                                    .sender_fee_tracker
                                    .get_total_fee_for_allocation(&allocation_id.address())
                                    .map(|fee| fee.value)
                                    .unwrap_or_default() as f64,
                            );
                    }
                    ReceiptFees::RavRequestResponse(fees, rav_result) => {
                        state.finalize_rav_request(allocation_id.address(), (fees, rav_result));
                    }
                    ReceiptFees::UpdateValue(unaggregated_fees) => {
                        state.update_sender_fee(allocation_id.address(), unaggregated_fees);
                    }
                    ReceiptFees::Retry => {}
                }

                // Eagerly deny the sender (if needed), before the RAV request. To be sure not to
                // delay the denial because of the RAV request, which could take some time.

                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }

                let has_available_slots_for_requests = state.adaptive_limiter.has_limit();
                if has_available_slots_for_requests {
                    let total_fee_outside_buffer = state.sender_fee_tracker.get_ravable_total_fee();
                    let total_counter_for_allocation = state
                        .sender_fee_tracker
                        .get_count_outside_buffer_for_allocation(&allocation_id.address());
                    let can_trigger_rav = state
                        .sender_fee_tracker
                        .can_trigger_rav(allocation_id.address());
                    let counter_greater_receipt_limit = total_counter_for_allocation
                        >= state.config.rav_request_receipt_limit
                        && can_trigger_rav;
                    let rav_result = if !state.backoff_info.in_backoff()
                        && total_fee_outside_buffer >= state.config.trigger_value
                    {
                        tracing::debug!(
                            total_fee_outside_buffer,
                            trigger_value = state.config.trigger_value,
                            "Total fee greater than the trigger value. Triggering RAV request"
                        );
                        state.rav_request_for_heaviest_allocation().await
                    } else if counter_greater_receipt_limit {
                        tracing::debug!(
                            total_counter_for_allocation,
                            rav_request_receipt_limit = state.config.rav_request_receipt_limit,
                            %allocation_id,
                            "Total counter greater than the receipt limit per rav. Triggering RAV request"
                        );
                        state
                            .rav_request_for_allocation(allocation_id.address())
                            .await
                    } else {
                        Ok(())
                    };
                    // In case we fail, we want our actor to keep running
                    if let Err(err) = rav_result {
                        tracing::error!(
                            error = %err,
                            "There was an error while requesting a RAV."
                        );
                    }
                }

                // Retry logic: Check if the deny condition is still met after RAV processing
                // This is crucial for stopping retries when RAV requests successfully resolve
                // the underlying issue (e.g., clearing unaggregated fees).
                match (state.denied, state.deny_condition_reached()) {
                    // Case: Sender was denied BUT deny condition no longer met
                    // This happens when a successful RAV request clears unaggregated fees,
                    // reducing total_potential_fees below the balance threshold.
                    // Action: Remove from denylist and stop retrying.
                    (true, false) => state.remove_from_denylist().await,

                    // Case: Sender still denied AND deny condition still met
                    // This happens when RAV requests fail or don't sufficiently reduce fees.
                    // Action: Schedule another retry to attempt RAV creation again.
                    (true, true) => {
                        // retry in a moment
                        state.scheduled_rav_request =
                            Some(myself.send_after(state.retry_interval, move || {
                                SenderAccountMessage::UpdateReceiptFees(
                                    allocation_id,
                                    ReceiptFees::Retry,
                                )
                            }));
                    }
                    _ => {}
                }
            }
            SenderAccountMessage::UpdateAllocationIds(allocation_ids) => {
                // Create new sender allocations
                let mut new_allocation_ids = state.allocation_ids.clone();
                for allocation_id in allocation_ids.difference(&state.allocation_ids) {
                    if let Err(error) = state
                        .create_sender_allocation(myself.clone(), *allocation_id)
                        .await
                    {
                        tracing::error!(
                            %error,
                            %allocation_id,
                            "There was an error while creating Sender Allocation."
                        );
                    } else {
                        new_allocation_ids.insert(*allocation_id);
                    }
                }

                let possibly_closed_allocations = state
                    .allocation_ids
                    .difference(&allocation_ids)
                    .collect::<HashSet<_>>();

                let really_closed = state
                    .check_closed_allocations(possibly_closed_allocations.clone())
                    .await
                    .inspect_err(|err| tracing::error!(error = %err, "There was an error while querying the subgraph for closed allocations"))
                    .unwrap_or_default();

                // Remove sender allocations
                for allocation_id in possibly_closed_allocations {
                    if really_closed.contains(&allocation_id.address()) {
                        if let Some(sender_handle) = ActorRef::<SenderAllocationMessage>::where_is(
                            state.format_sender_allocation(&allocation_id.address()),
                        ) {
                            tracing::trace!(%allocation_id, "SenderAccount shutting down SenderAllocation");
                            // we can not send a rav request to this allocation
                            // because it's gonna trigger the last rav
                            state
                                .sender_fee_tracker
                                .block_allocation_id(allocation_id.address());
                            sender_handle.stop(None);
                            new_allocation_ids.remove(allocation_id);
                        }
                    } else {
                        tracing::warn!(%allocation_id, "Missing allocation was not closed yet");
                    }
                }

                tracing::trace!(
                    old_ids= ?state.allocation_ids,
                    new_ids = ?new_allocation_ids,
                    "Updating allocation ids"
                );
                state.allocation_ids = new_allocation_ids;
            }
            SenderAccountMessage::NewAllocationId(allocation_id) => {
                if let Err(error) = state
                    .create_sender_allocation(myself.clone(), allocation_id)
                    .await
                {
                    tracing::error!(
                        %error,
                        %allocation_id,
                        "There was an error while creating Sender Allocation."
                    );
                }
                state.allocation_ids.insert(allocation_id);
            }
            SenderAccountMessage::UpdateBalanceAndLastRavs(new_balance, non_final_last_ravs) => {
                state.sender_balance = new_balance;
                ESCROW_BALANCE
                    .with_label_values(&[&state.sender.to_string()])
                    .set(new_balance.to_u128().expect("should be less than 128 bits") as f64);

                let non_final_last_ravs_set: HashSet<_> =
                    non_final_last_ravs.keys().cloned().collect();

                let active_allocation_ids = state
                    .allocation_ids
                    .iter()
                    .map(|id| id.address())
                    .collect::<HashSet<_>>()
                    .union(&non_final_last_ravs_set)
                    .cloned()
                    .collect::<HashSet<_>>();

                let tracked_allocation_ids = state.rav_tracker.get_list_of_allocation_ids();
                // all tracked ravs that are not in the current allocation_ids nor on the received list
                for allocation_id in tracked_allocation_ids.difference(&active_allocation_ids) {
                    // if it's being tracked and we didn't receive any update from the non_final_last_ravs
                    // remove from the tracker
                    state.rav_tracker.remove(*allocation_id);

                    let _ = PENDING_RAV.remove_label_values(&[
                        &state.sender.to_string(),
                        &allocation_id.to_string(),
                    ]);
                }

                for (allocation_id, value) in non_final_last_ravs {
                    state.update_rav(allocation_id, value);
                }
                // now that balance and rav tracker is updated, check
                match (state.denied, state.deny_condition_reached()) {
                    (true, false) => state.remove_from_denylist().await,
                    (false, true) => state.add_to_denylist().await,
                    (_, _) => {}
                }
            }
            #[cfg(test)]
            SenderAccountMessage::GetSenderFeeTracker(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.sender_fee_tracker.clone());
                }
            }
            #[cfg(test)]
            SenderAccountMessage::GetDeny(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.denied);
                }
            }
            #[cfg(test)]
            SenderAccountMessage::IsSchedulerEnabled(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.scheduled_rav_request.is_some());
                }
            }
        }
        Ok(())
    }

    /// We define the supervisor event to overwrite the default behavior which
    /// is shutdown the supervisor on actor termination events
    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::trace!(
            sender = %state.sender,
            message = ?message,
            "New SenderAccount supervision event"
        );

        match message {
            SupervisionEvent::ActorTerminated(cell, _, _) => {
                // what to do in case of termination or panic?
                let sender_allocation = cell.get_name();
                tracing::warn!(?sender_allocation, "Actor SenderAllocation was terminated");

                let Some(allocation_id) = cell.get_name() else {
                    tracing::error!("SenderAllocation doesn't have a name");
                    return Ok(());
                };
                let Some(allocation_id) = allocation_id.split(':').next_back() else {
                    tracing::error!(%allocation_id, "Could not extract allocation_id from name");
                    return Ok(());
                };
                let Ok(allocation_id) = Address::parse_checksummed(allocation_id, None) else {
                    tracing::error!(%allocation_id, "Could not convert allocation_id to Address");
                    return Ok(());
                };

                // remove from sender_fee_tracker
                state.sender_fee_tracker.remove(allocation_id);

                SENDER_FEE_TRACKER
                    .with_label_values(&[&state.sender.to_string()])
                    .set(state.sender_fee_tracker.get_total_fee() as f64);

                let _ = UNAGGREGATED_FEES
                    .remove_label_values(&[&state.sender.to_string(), &allocation_id.to_string()]);

                // check for deny conditions
                let _ = myself.cast(SenderAccountMessage::UpdateReceiptFees(
                    AllocationId::Legacy(AllocationIdCore::from(allocation_id)),
                    ReceiptFees::Retry,
                ));

                // rav tracker is not updated because it's still not redeemed
            }
            SupervisionEvent::ActorFailed(cell, error) => {
                let sender_allocation = cell.get_name();
                tracing::warn!(
                    ?sender_allocation,
                    ?error,
                    "Actor SenderAllocation failed. Restarting..."
                );
                let Some(allocation_id) = cell.get_name() else {
                    tracing::error!("SenderAllocation doesn't have a name");
                    return Ok(());
                };
                let Some(allocation_id) = allocation_id.split(':').next_back() else {
                    tracing::error!(%allocation_id, "Could not extract allocation_id from name");
                    return Ok(());
                };
                let Ok(allocation_id) = Address::parse_checksummed(allocation_id, None) else {
                    tracing::error!(%allocation_id, "Could not convert allocation_id to Address");
                    return Ok(());
                };
                let Some(allocation_id) = state
                    .allocation_ids
                    .iter()
                    .find(|id| id.address() == allocation_id)
                else {
                    tracing::error!(%allocation_id, "Could not get allocation id type from state");
                    return Ok(());
                };

                if let Err(error) = state
                    .create_sender_allocation(myself.clone(), *allocation_id)
                    .await
                {
                    tracing::error!(
                        %error,
                        %allocation_id,
                        "Error while recreating Sender Allocation."
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test"))]
impl SenderAccount {
    /// Deny sender by giving `sender` [Address]
    pub async fn deny_sender(sender_type: SenderType, pool: &PgPool, sender: Address) {
        match sender_type {
            SenderType::Legacy => Self::deny_v1_sender(pool, sender).await,
            SenderType::Horizon => Self::deny_v2_sender(pool, sender).await,
        }
    }

    async fn deny_v1_sender(pool: &PgPool, sender: Address) {
        sqlx::query!(
            r#"
                    INSERT INTO scalar_tap_denylist (sender_address)
                    VALUES ($1) ON CONFLICT DO NOTHING
                "#,
            sender.encode_hex(),
        )
        .execute(pool)
        .await
        .expect("Should not fail to insert into denylist");
    }

    async fn deny_v2_sender(pool: &PgPool, sender: Address) {
        sqlx::query!(
            r#"
                    INSERT INTO tap_horizon_denylist (sender_address)
                    VALUES ($1) ON CONFLICT DO NOTHING
                "#,
            sender.encode_hex(),
        )
        .execute(pool)
        .await
        .expect("Should not fail to insert into \"horizon\" denylist");
    }
}

#[cfg(test)]
pub mod tests {
    #![allow(missing_docs)]
    use std::{
        collections::{HashMap, HashSet},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use indexer_monitor::EscrowAccounts;
    use ractor::{call, Actor, ActorRef, ActorStatus};
    use serde_json::json;
    use test_assets::{
        flush_messages, ALLOCATION_ID_0, ALLOCATION_ID_1, TAP_SENDER as SENDER,
        TAP_SIGNER as SIGNER,
    };
    use thegraph_core::{
        alloy::{hex::ToHexExt, primitives::U256},
        AllocationId as AllocationIdCore,
    };
    use tokio::sync::mpsc;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use super::{RavInformation, SenderAccountMessage};
    use crate::{
        agent::{
            sender_account::ReceiptFees, sender_accounts_manager::AllocationId,
            sender_allocation::SenderAllocationMessage,
            unaggregated_receipts::UnaggregatedReceipts,
        },
        assert_not_triggered, assert_triggered,
        test::{
            actors::{create_mock_sender_allocation, MockSenderAllocation},
            create_rav, create_sender_account, store_rav_with_options, ESCROW_VALUE, TRIGGER_VALUE,
        },
    };

    /// Prefix shared between tests so we don't have conflicts in the global registry
    const BUFFER_DURATION: Duration = Duration::from_millis(100);
    const RETRY_DURATION: Duration = Duration::from_millis(1000);

    async fn setup_mock_escrow_subgraph() -> MockServer {
        let mock_escrow_subgraph_server: MockServer = MockServer::start().await;
        mock_escrow_subgraph_server
                .register(
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
        mock_escrow_subgraph_server
    }
    struct TestSenderAccount {
        sender_account: ActorRef<SenderAccountMessage>,
        msg_receiver: mpsc::Receiver<SenderAccountMessage>,
        prefix: String,
    }

    #[tokio::test]
    async fn test_update_allocation_ids() {
        let mock_escrow_subgraph = setup_mock_escrow_subgraph().await;
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        let no_allocations_closed_guard = mock_server
            .register_as_scoped(
                Mock::given(method("POST"))
                    .and(body_string_contains("ClosedAllocations"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                            "meta": {
                                "block": {
                                    "number": 1,
                                    "hash": "hash",
                                    "timestamp": 1
                                }
                            },
                            "allocations": []
                        }
                    }))),
            )
            .await;

        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .escrow_subgraph_endpoint(&mock_escrow_subgraph.uri())
            .network_subgraph_endpoint(&mock_server.uri())
            .call()
            .await;

        let allocation_ids = HashSet::from_iter([AllocationId::Legacy(AllocationIdCore::from(
            ALLOCATION_ID_0,
        ))]);
        // we expect it to create a sender allocation
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(
                allocation_ids.clone(),
            ))
            .unwrap();
        let message = msg_receiver.recv().await.expect("Channel failed");
        insta::assert_debug_snapshot!(message);

        // verify if create sender account
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, ALLOCATION_ID_0);
        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_some());

        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();
        let message = msg_receiver.recv().await.expect("Channel failed");
        insta::assert_debug_snapshot!(message);

        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_some());

        drop(no_allocations_closed_guard);
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("ClosedAllocations"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                            "meta": {
                                "block": {
                                    "number": 1,
                                    "hash": "hash",
                                    "timestamp": 1
                                }
                            },
                            "allocations": [
                                {"id": ALLOCATION_ID_0 }
                            ]
                        }
                    }))),
            )
            .await;

        // try to delete sender allocation_id
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();
        let msg = msg_receiver.recv().await.expect("Channel failed");
        insta::assert_debug_snapshot!(msg);

        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_none());
    }

    #[tokio::test]
    async fn test_new_allocation_id() {
        let mock_escrow_subgraph = setup_mock_escrow_subgraph().await;
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        let no_closed = mock_server
            .register_as_scoped(
                Mock::given(method("POST"))
                    .and(body_string_contains("ClosedAllocations"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                            "meta": {
                                "block": {
                                    "number": 1,
                                    "hash": "hash",
                                    "timestamp": 1
                                }
                            },
                            "allocations": []
                        }
                    }))),
            )
            .await;

        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .escrow_subgraph_endpoint(&mock_escrow_subgraph.uri())
            .network_subgraph_endpoint(&mock_server.uri())
            .call()
            .await;

        // we expect it to create a sender allocation
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(AllocationId::Legacy(
                AllocationIdCore::from(ALLOCATION_ID_0),
            )))
            .unwrap();

        flush_messages(&mut msg_receiver).await;

        // verify if create sender account
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, ALLOCATION_ID_0);
        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_some());

        // nothing should change because we already created
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(
                vec![AllocationId::Legacy(AllocationIdCore::from(
                    ALLOCATION_ID_0,
                ))]
                .into_iter()
                .collect(),
            ))
            .unwrap();

        flush_messages(&mut msg_receiver).await;

        // try to delete sender allocation_id
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();

        flush_messages(&mut msg_receiver).await;

        // should not delete it because it was not in network subgraph
        let allocation_ref =
            ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone()).unwrap();

        // Mock result for closed allocations

        drop(no_closed);
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("ClosedAllocations"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                            "meta": {
                                "block": {
                                    "number": 1,
                                    "hash": "hash",
                                    "timestamp": 1
                                }
                            },
                            "allocations": [
                                {"id": ALLOCATION_ID_0 }
                            ]
                        }
                    }))),
            )
            .await;

        // try to delete sender allocation_id
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();

        allocation_ref.wait(None).await.unwrap();

        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_none());

        // safely stop the manager
        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    fn get_current_timestamp_u64_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[tokio::test]
    async fn test_update_receipt_fees_no_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let (sender_account, msg_receiver, prefix, _) =
            create_sender_account().pgpool(pgpool).call().await;
        let basic_sender_account = TestSenderAccount {
            sender_account,
            msg_receiver,
            prefix,
        };
        // create a fake sender allocation
        let (triggered_rav_request, _, _) = create_mock_sender_allocation(
            basic_sender_account.prefix,
            SENDER.1,
            ALLOCATION_ID_0,
            basic_sender_account.sender_account.clone(),
        )
        .await;

        basic_sender_account
            .sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(TRIGGER_VALUE - 1, get_current_timestamp_u64_ns()),
            ))
            .unwrap();

        // wait the buffer
        tokio::time::sleep(BUFFER_DURATION).await;

        assert_not_triggered!(&triggered_rav_request);
    }

    #[tokio::test]
    async fn test_update_receipt_fees_trigger_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let (sender_account, msg_receiver, prefix, _) =
            create_sender_account().pgpool(pgpool).call().await;
        let mut basic_sender_account = TestSenderAccount {
            sender_account,
            msg_receiver,
            prefix,
        };
        // create a fake sender allocation
        let (triggered_rav_request, _, _) = create_mock_sender_allocation(
            basic_sender_account.prefix,
            SENDER.1,
            ALLOCATION_ID_0,
            basic_sender_account.sender_account.clone(),
        )
        .await;

        basic_sender_account
            .sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
            ))
            .unwrap();

        flush_messages(&mut basic_sender_account.msg_receiver).await;
        assert_not_triggered!(&triggered_rav_request);

        // wait for it to be outside buffer
        tokio::time::sleep(BUFFER_DURATION).await;

        basic_sender_account
            .sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::Retry,
            ))
            .unwrap();
        flush_messages(&mut basic_sender_account.msg_receiver).await;

        assert_triggered!(&triggered_rav_request);
    }

    #[tokio::test]
    async fn test_counter_greater_limit_trigger_rav() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool.clone())
            .rav_request_receipt_limit(2)
            .call()
            .await;

        // create a fake sender allocation
        let (triggered_rav_request, _, _) = create_mock_sender_allocation(
            prefix,
            SENDER.1,
            ALLOCATION_ID_0,
            sender_account.clone(),
        )
        .await;

        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(1, get_current_timestamp_u64_ns()),
            ))
            .unwrap();
        flush_messages(&mut msg_receiver).await;

        assert_not_triggered!(&triggered_rav_request);

        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(1, get_current_timestamp_u64_ns()),
            ))
            .unwrap();
        flush_messages(&mut msg_receiver).await;

        // wait for it to be outside buffer
        tokio::time::sleep(BUFFER_DURATION).await;

        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::Retry,
            ))
            .unwrap();
        flush_messages(&mut msg_receiver).await;

        assert_triggered!(&triggered_rav_request);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn test_remove_sender_account() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let mock_escrow_subgraph = setup_mock_escrow_subgraph().await;
        let (sender_account, _, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .initial_allocation(
                vec![AllocationId::Legacy(AllocationIdCore::from(
                    ALLOCATION_ID_0,
                ))]
                .into_iter()
                .collect(),
            )
            .escrow_subgraph_endpoint(&mock_escrow_subgraph.uri())
            .call()
            .await;

        // check if allocation exists
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, ALLOCATION_ID_0);
        let Some(sender_allocation) =
            ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone())
        else {
            panic!("Sender allocation was not created");
        };

        // stop
        sender_account.stop_and_wait(None, None).await.unwrap();

        // check if sender_account is stopped
        assert_eq!(sender_account.get_status(), ActorStatus::Stopped);

        // check if sender_allocation is also stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);
    }

    /// Test that the deny status is correctly loaded from the DB at the start of the actor
    #[rstest::rstest]
    #[tokio::test]
    async fn test_init_deny() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            SENDER.1.encode_hex(),
        )
        .execute(&pgpool)
        .await
        .expect("Should not fail to insert into denylist");

        // make sure there's a reason to keep denied
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        let (sender_account, _notify, _, _) =
            create_sender_account().pgpool(pgpool.clone()).call().await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny);
    }

    /// Tests the retry mechanism for RAV requests when a sender is blocked due to unaggregated fees.
    ///
    /// This test verifies that:
    /// 1. When unaggregated fees exceed the allowed limit, the sender enters a retry state
    /// 2. The retry mechanism triggers RAV requests to resolve the blocked condition
    /// 3. When a RAV request succeeds and clears unaggregated fees, retries stop appropriately
    ///
    /// Key behavior tested:
    /// - Sender is blocked when max_unaggregated_fees_per_sender = 0 and any fees are added
    /// - First retry attempt triggers a RAV request
    /// - Successful RAV request clears unaggregated fees and creates a RAV for the amount
    /// - No additional retries occur since the deny condition is resolved
    ///
    /// This aligns with the TAP protocol where RAV creation aggregates unaggregated receipts
    /// into a voucher, effectively clearing the unaggregated fees balance.
    #[tokio::test]
    async fn test_retry_unaggregated_fees() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // we set to zero to block the sender, no matter the fee
        let max_unaggregated_fees_per_sender: u128 = 0;

        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .max_amount_willing_to_lose_grt(max_unaggregated_fees_per_sender)
            .call()
            .await;

        let (triggered_rav_request, next_value, _) = create_mock_sender_allocation(
            prefix,
            SENDER.1,
            ALLOCATION_ID_0,
            sender_account.clone(),
        )
        .await;

        assert_not_triggered!(&triggered_rav_request);

        next_value.send(TRIGGER_VALUE).unwrap();

        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
            ))
            .unwrap();
        flush_messages(&mut msg_receiver).await;

        // wait to try again so it's outside the buffer
        tokio::time::sleep(RETRY_DURATION).await;
        assert_triggered!(triggered_rav_request);

        // Verify that no additional retry happens since the first RAV request
        // successfully cleared the unaggregated fees and resolved the deny condition.
        // This validates that the retry mechanism stops when the underlying issue is resolved,
        // which is the correct behavior according to the TAP protocol and retry logic.
        tokio::time::sleep(RETRY_DURATION).await;
        assert_not_triggered!(triggered_rav_request);
    }

    #[tokio::test]
    async fn test_deny_allow() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
            call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
        }

        let max_unaggregated_fees_per_sender: u128 = 1000;

        // Making sure no RAV is going to be triggered during the test
        let (sender_account, mut msg_receiver, _, _) = create_sender_account()
            .pgpool(pgpool.clone())
            .rav_request_trigger_value(u128::MAX)
            .max_amount_willing_to_lose_grt(max_unaggregated_fees_per_sender)
            .call()
            .await;

        macro_rules! update_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                        ReceiptFees::UpdateValue(UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                            counter: 0,
                        }),
                    ))
                    .unwrap();

                flush_messages(&mut msg_receiver).await;
            };
        }

        macro_rules! update_invalid_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                        AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                        UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                            counter: 0,
                        },
                    ))
                    .unwrap();

                flush_messages(&mut msg_receiver).await;
            };
        }

        update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny);

        update_receipt_fees!(max_unaggregated_fees_per_sender);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny);

        update_receipt_fees!(max_unaggregated_fees_per_sender + 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny);

        update_receipt_fees!(0);

        update_invalid_receipt_fees!(max_unaggregated_fees_per_sender - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny);

        update_invalid_receipt_fees!(max_unaggregated_fees_per_sender);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        // invalid receipts should not go down
        update_invalid_receipt_fees!(0);
        let deny = get_deny_status(&sender_account).await;
        // keep denied
        assert!(deny);

        // condition reached using receipts
        update_receipt_fees!(0);
        let deny = get_deny_status(&sender_account).await;
        // allow sender
        assert!(!deny);

        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_initialization_with_pending_ravs_over_the_limit() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // add last non-final ravs
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        let (sender_account, _notify, _, _) = create_sender_account()
            .pgpool(pgpool.clone())
            .max_amount_willing_to_lose_grt(u128::MAX)
            .call()
            .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny);
    }

    #[tokio::test]
    async fn test_unaggregated_fees_over_balance() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // add last non-final ravs
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        // other rav final, should not be taken into account
        let signed_rav = create_rav(ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(true)
            .call()
            .await
            .unwrap();

        let trigger_rav_request = ESCROW_VALUE * 2;

        // initialize with no trigger value and no max receipt deny
        let (sender_account, mut msg_receiver, prefix, escrow_accounts_tx) =
            create_sender_account()
                .pgpool(pgpool.clone())
                .rav_request_trigger_value(trigger_rav_request)
                .max_amount_willing_to_lose_grt(u128::MAX)
                .call()
                .await;

        // Set up proper escrow balance for the test
        escrow_accounts_tx
            .send(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            ))
            .unwrap();

        let (mock_sender_allocation, next_rav_value) =
            MockSenderAllocation::new_with_next_rav_value(sender_account.clone());

        let name = format!("{}:{}:{}", prefix, SENDER.1, ALLOCATION_ID_0);
        let (allocation, _) = MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
            .await
            .unwrap();

        async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
            call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
        }

        macro_rules! update_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                        ReceiptFees::UpdateValue(UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                            counter: 0,
                        }),
                    ))
                    .unwrap();

                flush_messages(&mut msg_receiver).await;
            };
        }

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny);

        let half_escrow = ESCROW_VALUE / 2;
        update_receipt_fees!(half_escrow);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        update_receipt_fees!(half_escrow - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny);

        update_receipt_fees!(half_escrow + 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        update_receipt_fees!(half_escrow + 2);
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        // trigger rav request
        // set the unnagregated fees to zero and the rav to the amount
        next_rav_value.send(trigger_rav_request).unwrap();

        update_receipt_fees!(trigger_rav_request);

        // receipt fees should already be 0, but we are setting to 0 again
        update_receipt_fees!(0);

        // should stay denied because the value was transfered to rav
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        allocation.stop_and_wait(None, None).await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_trusted_sender() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let max_amount_willing_to_lose_grt = ESCROW_VALUE / 10;
        // initialize with no trigger value and no max receipt deny
        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .trusted_sender(true)
            .rav_request_trigger_value(u128::MAX)
            .max_amount_willing_to_lose_grt(max_amount_willing_to_lose_grt)
            .call()
            .await;

        let (mock_sender_allocation, _, _) =
            MockSenderAllocation::new_with_triggered_rav_request(sender_account.clone());

        let name = format!("{}:{}:{}", prefix, SENDER.1, ALLOCATION_ID_0);
        let (allocation, _) = MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
            .await
            .unwrap();

        async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
            call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
        }

        macro_rules! update_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateRav(RavInformation {
                        allocation_id: ALLOCATION_ID_0,
                        value_aggregate: $value,
                    }))
                    .unwrap();

                flush_messages(&mut msg_receiver).await;
            };
        }

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny);

        update_receipt_fees!(ESCROW_VALUE - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(!deny, "it shouldn't deny a sender below escrow balance");

        update_receipt_fees!(ESCROW_VALUE);
        let deny = get_deny_status(&sender_account).await;
        assert!(
            !deny,
            "it shouldn't deny a trusted sender below escrow balance + max willing to lose"
        );

        update_receipt_fees!(ESCROW_VALUE + max_amount_willing_to_lose_grt - 1);
        let deny = get_deny_status(&sender_account).await;
        assert!(
            !deny,
            "it shouldn't deny a trusted sender below escrow balance + max willing to lose"
        );

        update_receipt_fees!(ESCROW_VALUE + max_amount_willing_to_lose_grt);
        let deny = get_deny_status(&sender_account).await;
        assert!(
            deny,
            "it should deny a trusted sender over escrow balance + max willing to lose"
        );

        allocation.stop_and_wait(None, None).await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_pending_rav_already_redeemed_and_redeem() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "transactions": [
                            {"allocationID": ALLOCATION_ID_0 }
                        ]}}),
                    )),
            )
            .await;

        // redeemed
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        let signed_rav = create_rav(ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE - 1);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        let (sender_account, mut msg_receiver, _, escrow_accounts_tx) = create_sender_account()
            .pgpool(pgpool.clone())
            .max_amount_willing_to_lose_grt(u128::MAX)
            .escrow_subgraph_endpoint(&mock_server.uri())
            .call()
            .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should start unblocked");

        mock_server.reset().await;

        // allocation_id sent to the blockchain
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "transactions": [
                            {"allocationID": ALLOCATION_ID_0 },
                            {"allocationID": ALLOCATION_ID_1 }
                        ]}}),
                    )),
            )
            .await;
        // escrow_account updated
        escrow_accounts_tx
            .send(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(1))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            ))
            .unwrap();

        // wait the actor react to the messages
        flush_messages(&mut msg_receiver).await;

        // should still be active with a 1 escrow available

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should keep unblocked");

        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_thawing_deposit_process() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // add last non-final ravs
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options()
            .pgpool(&pgpool)
            .signed_rav(signed_rav)
            .sender(SENDER.1)
            .last(true)
            .final_rav(false)
            .call()
            .await
            .unwrap();

        let (sender_account, mut msg_receiver, _, escrow_accounts_tx) = create_sender_account()
            .pgpool(pgpool.clone())
            .max_amount_willing_to_lose_grt(u128::MAX)
            .call()
            .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should start unblocked");

        // update the escrow to a lower value
        escrow_accounts_tx
            .send(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE / 2))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            ))
            .unwrap();

        flush_messages(&mut msg_receiver).await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny, "should block the sender");

        // simulate deposit
        escrow_accounts_tx
            .send(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            ))
            .unwrap();

        flush_messages(&mut msg_receiver).await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should unblock the sender");

        sender_account.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_sender_denied_close_allocation_stop_retry() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // we set to 1 to block the sender on a really low value
        let max_unaggregated_fees_per_sender: u128 = 1;

        let (sender_account, mut msg_receiver, prefix, _) = create_sender_account()
            .pgpool(pgpool)
            .max_amount_willing_to_lose_grt(max_unaggregated_fees_per_sender)
            .call()
            .await;

        let (mock_sender_allocation, _, next_unaggregated_fees) =
            MockSenderAllocation::new_with_triggered_rav_request(sender_account.clone());

        let name = format!("{}:{}:{}", prefix, SENDER.1, ALLOCATION_ID_0);
        let (allocation, _) = MockSenderAllocation::spawn_linked(
            Some(name),
            mock_sender_allocation,
            (),
            sender_account.get_cell(),
        )
        .await
        .unwrap();
        next_unaggregated_fees.send(TRIGGER_VALUE).unwrap();

        // set retry
        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
            ))
            .unwrap();
        let msg = msg_receiver.recv().await.expect("Channel failed");
        assert!(matches!(
            msg,
            SenderAccountMessage::UpdateReceiptFees(
                AllocationId::Legacy(allocation_id),
                ReceiptFees::NewReceipt(TRIGGER_VALUE, _)
            ) if allocation_id == AllocationIdCore::from(ALLOCATION_ID_0)
        ));

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny, "should be blocked");

        let scheduler_enabled =
            call!(sender_account, SenderAccountMessage::IsSchedulerEnabled).unwrap();
        assert!(scheduler_enabled, "should have an scheduler enabled");

        // close the allocation and trigger
        allocation.stop_and_wait(None, None).await.unwrap();

        // should remove the block and the retry
        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should be unblocked");

        let scheuduler_enabled =
            call!(sender_account, SenderAccountMessage::IsSchedulerEnabled).unwrap();
        assert!(!scheuduler_enabled, "should have an scheduler disabled");

        sender_account.stop_and_wait(None, None).await.unwrap();
    }
}
