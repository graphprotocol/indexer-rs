// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::hex::ToHexExt;
use alloy::primitives::U256;

use bigdecimal::num_bigint::ToBigInt;
use bigdecimal::ToPrimitive;

use futures::{stream, StreamExt};
use indexer_query::unfinalized_transactions;
use indexer_query::{
    closed_allocations::{self, ClosedAllocations},
    UnfinalizedTransactions,
};
use indexer_watcher::watch_pipe;
use jsonrpsee::http_client::HttpClientBuilder;
use prometheus::{register_gauge_vec, register_int_gauge_vec, GaugeVec, IntGaugeVec};
use reqwest::Url;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::watch::Receiver;
use tokio::task::JoinHandle;

use alloy::dyn_abi::Eip712Domain;
use alloy::primitives::Address;
use anyhow::Result;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use ractor::{Actor, ActorProcessingErr, ActorRef, MessagingErr, SupervisionEvent};
use sqlx::PgPool;
use tap_core::rav::SignedRAV;
use tracing::{error, warn, Level};

use super::sender_allocation::{SenderAllocation, SenderAllocationArgs};
use crate::adaptative_concurrency::AdaptiveLimiter;
use crate::agent::sender_allocation::{AllocationConfig, SenderAllocationMessage};
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::backoff::BackoffInfo;
use crate::tracker::{SenderFeeTracker, SimpleFeeTracker};
use lazy_static::lazy_static;

#[cfg(test)]
pub mod tests;

lazy_static! {
    static ref SENDER_DENIED: IntGaugeVec =
        register_int_gauge_vec!("tap_sender_denied", "Sender is denied", &["sender"]).unwrap();
    static ref ESCROW_BALANCE: GaugeVec = register_gauge_vec!(
        "tap_sender_escrow_balance_grt_total",
        "Sender escrow balance",
        &["sender"]
    )
    .unwrap();
    static ref UNAGGREGATED_FEES: GaugeVec = register_gauge_vec!(
        "tap_unaggregated_fees_grt_total",
        "Unggregated Fees value",
        &["sender", "allocation"]
    )
    .unwrap();
    static ref SENDER_FEE_TRACKER: GaugeVec = register_gauge_vec!(
        "tap_sender_fee_tracker_grt_total",
        "Sender fee tracker metric",
        &["sender"]
    )
    .unwrap();
    static ref INVALID_RECEIPT_FEES: GaugeVec = register_gauge_vec!(
        "tap_invalid_receipt_fees_grt_total",
        "Failed receipt fees",
        &["sender", "allocation"]
    )
    .unwrap();
    static ref PENDING_RAV: GaugeVec = register_gauge_vec!(
        "tap_pending_rav_grt_total",
        "Pending ravs values",
        &["sender", "allocation"]
    )
    .unwrap();
    static ref MAX_FEE_PER_SENDER: GaugeVec = register_gauge_vec!(
        "tap_max_fee_per_sender_grt_total",
        "Max fee per sender in the config",
        &["sender"]
    )
    .unwrap();
    static ref RAV_REQUEST_TRIGGER_VALUE: GaugeVec = register_gauge_vec!(
        "tap_rav_request_trigger_value",
        "RAV request trigger value divisor",
        &["sender"]
    )
    .unwrap();
}

const INITIAL_RAV_REQUEST_CONCURRENT: usize = 1;

type RavMap = HashMap<Address, u128>;
type Balance = U256;

#[derive(Debug)]
pub enum ReceiptFees {
    NewReceipt(u128, u64),
    UpdateValue(UnaggregatedReceipts),
    RavRequestResponse((UnaggregatedReceipts, anyhow::Result<Option<SignedRAV>>)),
    Retry,
}

#[derive(Debug)]
pub enum SenderAccountMessage {
    UpdateBalanceAndLastRavs(Balance, RavMap),
    UpdateAllocationIds(HashSet<Address>),
    NewAllocationId(Address),
    UpdateReceiptFees(Address, ReceiptFees),
    UpdateInvalidReceiptFees(Address, UnaggregatedReceipts),
    UpdateRav(SignedRAV),
    #[cfg(test)]
    GetSenderFeeTracker(ractor::RpcReplyPort<SenderFeeTracker>),
    #[cfg(test)]
    GetDeny(ractor::RpcReplyPort<bool>),
    #[cfg(test)]
    IsSchedulerEnabled(ractor::RpcReplyPort<bool>),
}

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

pub struct SenderAccountArgs {
    pub config: &'static SenderAccountConfig,

    pub pgpool: PgPool,
    pub sender_id: Address,
    pub escrow_accounts: Receiver<EscrowAccounts>,
    pub indexer_allocations: Receiver<HashSet<Address>>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub network_subgraph: &'static SubgraphClient,
    pub domain_separator: Eip712Domain,
    pub sender_aggregator_endpoint: Url,
    pub allocation_ids: HashSet<Address>,
    pub prefix: Option<String>,

    pub retry_interval: Duration,
}
pub struct State {
    prefix: Option<String>,
    sender_fee_tracker: SenderFeeTracker,
    rav_tracker: SimpleFeeTracker,
    invalid_receipts_tracker: SimpleFeeTracker,
    allocation_ids: HashSet<Address>,
    _indexer_allocations_handle: JoinHandle<()>,
    _escrow_account_monitor: JoinHandle<()>,
    scheduled_rav_request: Option<JoinHandle<Result<(), MessagingErr<SenderAccountMessage>>>>,

    sender: Address,

    // Deny reasons
    denied: bool,
    sender_balance: U256,
    retry_interval: Duration,

    // concurrent rav request
    adaptive_limiter: AdaptiveLimiter,

    // Receivers
    escrow_accounts: Receiver<EscrowAccounts>,

    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,

    domain_separator: Eip712Domain,
    pgpool: PgPool,
    sender_aggregator: jsonrpsee::http_client::HttpClient,

    // Backoff info
    backoff_info: BackoffInfo,

    // Config
    config: &'static SenderAccountConfig,
}

pub struct SenderAccountConfig {
    pub rav_request_buffer: Duration,
    pub max_amount_willing_to_lose_grt: u128,
    pub trigger_value: u128,

    // allocation config
    pub rav_request_timeout: Duration,
    pub rav_request_receipt_limit: u64,
    pub indexer_address: Address,
    pub escrow_polling_interval: Duration,
}

impl SenderAccountConfig {
    pub fn from_config(config: &indexer_config::Config) -> Self {
        Self {
            rav_request_buffer: config.tap.rav_request.timestamp_buffer_secs,
            rav_request_receipt_limit: config.tap.rav_request.max_receipts_per_request,
            indexer_address: config.indexer.indexer_address,
            escrow_polling_interval: config.subgraphs.escrow.config.syncing_interval_secs,
            max_amount_willing_to_lose_grt: config.tap.max_amount_willing_to_lose_grt.get_value(),
            trigger_value: config.tap.get_trigger_value(),
            rav_request_timeout: config.tap.rav_request.request_timeout_secs,
        }
    }
}

impl State {
    async fn create_sender_allocation(
        &self,
        sender_account_ref: ActorRef<SenderAccountMessage>,
        allocation_id: Address,
    ) -> Result<()> {
        tracing::trace!(
            %self.sender,
            %allocation_id,
            "SenderAccount is creating allocation."
        );
        let args = SenderAllocationArgs {
            pgpool: self.pgpool.clone(),
            allocation_id,
            sender: self.sender,
            escrow_accounts: self.escrow_accounts.clone(),
            escrow_subgraph: self.escrow_subgraph,
            domain_separator: self.domain_separator.clone(),
            sender_account_ref: sender_account_ref.clone(),
            sender_aggregator: self.sender_aggregator.clone(),
            config: AllocationConfig::from_sender_config(self.config),
        };

        SenderAllocation::spawn_linked(
            Some(self.format_sender_allocation(&allocation_id)),
            SenderAllocation,
            args,
            sender_account_ref.get_cell(),
        )
        .await?;
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

    async fn rav_request_for_heaviest_allocation(&mut self) -> Result<()> {
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

    async fn rav_request_for_allocation(&mut self, allocation_id: Address) -> Result<()> {
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation actor {allocation_id}");
        };

        allocation
            .cast(SenderAllocationMessage::TriggerRAVRequest)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Error while sending and waiting message for actor {allocation_id}. Error: {e}"
                )
            })?;
        self.adaptive_limiter.acquire();
        self.sender_fee_tracker.start_rav_request(allocation_id);

        Ok(())
    }

    fn finalize_rav_request(
        &mut self,
        allocation_id: Address,
        rav_response: (UnaggregatedReceipts, anyhow::Result<Option<SignedRAV>>),
    ) {
        self.sender_fee_tracker.finish_rav_request(allocation_id);
        let (fees, rav_result) = rav_response;
        match rav_result {
            Ok(signed_rav) => {
                self.sender_fee_tracker.ok_rav_request(allocation_id);
                self.adaptive_limiter.on_success();
                let rav_value = signed_rav.map_or(0, |rav| rav.message.valueAggregate);
                self.update_rav(allocation_id, rav_value);
            }
            Err(err) => {
                self.sender_fee_tracker.failed_rav_backoff(allocation_id);
                self.adaptive_limiter.on_failure();
                error!(
                    "Error while requesting RAV for sender {} and allocation {}: {}",
                    self.sender, allocation_id, err
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

    fn deny_condition_reached(&self) -> bool {
        let pending_ravs = self.rav_tracker.get_total_fee();
        let unaggregated_fees = self.sender_fee_tracker.get_total_fee();
        let pending_fees_over_balance =
            U256::from(pending_ravs + unaggregated_fees) >= self.sender_balance;
        let max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt;
        let invalid_receipt_fees = self.invalid_receipts_tracker.get_total_fee();
        let total_fee_over_max_value =
            unaggregated_fees + invalid_receipt_fees >= max_amount_willing_to_lose;

        tracing::trace!(
            %pending_fees_over_balance,
            %total_fee_over_max_value,
            "Verifying if deny condition was reached.",
        );

        total_fee_over_max_value || pending_fees_over_balance
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    async fn add_to_denylist(&mut self) {
        tracing::warn!(
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt,
            sender_balance = self.sender_balance.to_u128(),
            "Denying sender."
        );

        SenderAccount::deny_sender(&self.pgpool, self.sender).await;
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
        self.denied = false;

        SENDER_DENIED
            .with_label_values(&[&self.sender.to_string()])
            .set(0);
    }

    /// receives a list of possible closed allocations and verify
    /// if they are really closed
    async fn check_closed_allocations(
        &self,
        allocation_ids: HashSet<&Address>,
    ) -> anyhow::Result<HashSet<Address>> {
        if allocation_ids.is_empty() {
            return Ok(HashSet::new());
        }
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
            .collect::<Result<HashSet<Address>, _>>()?)
    }
}

#[async_trait::async_trait]
impl Actor for SenderAccount {
    type Msg = SenderAccountMessage;
    type State = State;
    type Arguments = SenderAccountArgs;

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
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let myself_clone = myself.clone();
        let _indexer_allocations_handle = watch_pipe(indexer_allocations, move |allocation_ids| {
            let allocation_ids = allocation_ids.clone();
            // Update the allocation_ids
            myself_clone
                .cast(SenderAccountMessage::UpdateAllocationIds(allocation_ids))
                .unwrap_or_else(|e| {
                    error!("Error while updating allocation_ids: {:?}", e);
                });
            async {}
        });

        let myself_clone = myself.clone();
        let pgpool_clone = pgpool.clone();
        let accounts_clone = escrow_accounts.clone();
        let _escrow_account_monitor = watch_pipe(accounts_clone, move |escrow_account| {
            let myself = myself_clone.clone();
            let pgpool = pgpool_clone.clone();
            // Get balance or default value for sender
            // this balance already takes into account thawing
            let balance = escrow_account
                .get_balance_for_sender(&sender_id)
                .unwrap_or_default();
            async move {
                let last_non_final_ravs = sqlx::query!(
                    r#"
                            SELECT allocation_id, value_aggregate
                            FROM scalar_tap_ravs
                            WHERE sender_address = $1 AND last AND NOT final;
                        "#,
                    sender_id.encode_hex(),
                )
                .fetch_all(&pgpool)
                .await
                .expect("Should not fail to fetch from scalar_tap_ravs");

                // get a list from the subgraph of which subgraphs were already redeemed and were not marked as final
                let redeemed_ravs_allocation_ids = match escrow_subgraph
                    .query::<UnfinalizedTransactions, _>(unfinalized_transactions::Variables {
                        unfinalized_ravs_allocation_ids: last_non_final_ravs
                            .iter()
                            .map(|rav| rav.allocation_id.to_string())
                            .collect::<Vec<_>>(),
                        sender: format!("{:x?}", sender_id),
                    })
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
                };

                // filter the ravs marked as last that were not redeemed yet
                let non_redeemed_ravs = last_non_final_ravs
                    .into_iter()
                    .filter_map(|rav| {
                        Some((
                            Address::from_str(&rav.allocation_id).ok()?,
                            rav.value_aggregate.to_bigint().and_then(|v| v.to_u128())?,
                        ))
                    })
                    .filter(|(allocation, _value)| {
                        !redeemed_ravs_allocation_ids.contains(&format!("{:x?}", allocation))
                    })
                    .collect::<HashMap<_, _>>();

                // Update the allocation_ids
                myself
                    .cast(SenderAccountMessage::UpdateBalanceAndLastRavs(
                        balance,
                        non_redeemed_ravs,
                    ))
                    .unwrap_or_else(|e| {
                        error!(
                            "Error while updating balance for sender {}: {:?}",
                            sender_id, e
                        );
                    });
            }
        });

        // Get deny status from the scalar_tap_denylist table
        let denied = sqlx::query!(
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
        .expect("Deny status cannot be null");

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

        let sender_aggregator = HttpClientBuilder::default()
            .request_timeout(config.rav_request_timeout)
            .build(&sender_aggregator_endpoint)?;

        let state = State {
            prefix,
            sender_fee_tracker: SenderFeeTracker::new(config.rav_request_buffer),
            rav_tracker: SimpleFeeTracker::default(),
            invalid_receipts_tracker: SimpleFeeTracker::default(),
            allocation_ids: allocation_ids.clone(),
            _indexer_allocations_handle,
            _escrow_account_monitor,
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
            sender_aggregator,
            backoff_info: BackoffInfo::default(),
            config,
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

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
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
            SenderAccountMessage::UpdateRav(rav) => {
                state.update_rav(rav.message.allocationId, rav.message.valueAggregate);

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
                    .update(allocation_id, unaggregated_fees.value);

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
                            SenderAccount::deny_sender(&state.pgpool, state.sender).await;
                        }

                        // add new value
                        state
                            .sender_fee_tracker
                            .add(allocation_id, value, timestamp_ns);

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
                                    .get_total_fee_for_allocation(&allocation_id)
                                    .map(|fee| fee.value)
                                    .unwrap_or_default() as f64,
                            );
                    }
                    ReceiptFees::RavRequestResponse(rav_result) => {
                        state.finalize_rav_request(allocation_id, rav_result);
                    }
                    ReceiptFees::UpdateValue(unaggregated_fees) => {
                        state.update_sender_fee(allocation_id, unaggregated_fees);
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
                        .get_count_outside_buffer_for_allocation(&allocation_id);
                    let can_trigger_rav = state.sender_fee_tracker.can_trigger_rav(allocation_id);
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
                        state.rav_request_for_allocation(allocation_id).await
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

                match (state.denied, state.deny_condition_reached()) {
                    // Allow the sender right after the potential RAV request. This way, the
                    // sender can be allowed again as soon as possible if the RAV was successful.
                    (true, false) => state.remove_from_denylist().await,
                    // if couldn't remove from denylist, resend the message in 30 seconds
                    // this may trigger another rav request
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
                        error!(
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
                    .inspect_err(|err| error!(error = %err, "There was an error while querying the subgraph for closed allocations"))
                    .unwrap_or_default();

                // Remove sender allocations
                for allocation_id in possibly_closed_allocations {
                    if really_closed.contains(allocation_id) {
                        if let Some(sender_handle) = ActorRef::<SenderAllocationMessage>::where_is(
                            state.format_sender_allocation(allocation_id),
                        ) {
                            tracing::trace!(%allocation_id, "SenderAccount shutting down SenderAllocation");
                            // we can not send a rav request to this allocation
                            // because it's gonna trigger the last rav
                            state.sender_fee_tracker.block_allocation_id(*allocation_id);
                            sender_handle.stop(None);
                            new_allocation_ids.remove(allocation_id);
                        }
                    } else {
                        warn!(%allocation_id, "Missing allocation was not closed yet");
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
                    error!(
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

    // we define the supervisor event to overwrite the default behavior which
    // is shutdown the supervisor on actor termination events
    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
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
                let Some(allocation_id) = allocation_id.split(':').last() else {
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
                    allocation_id,
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
                let Some(allocation_id) = allocation_id.split(':').last() else {
                    tracing::error!(%allocation_id, "Could not extract allocation_id from name");
                    return Ok(());
                };
                let Ok(allocation_id) = Address::parse_checksummed(allocation_id, None) else {
                    tracing::error!(%allocation_id, "Could not convert allocation_id to Address");
                    return Ok(());
                };

                if let Err(error) = state
                    .create_sender_allocation(myself.clone(), allocation_id)
                    .await
                {
                    error!(
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

impl SenderAccount {
    pub async fn deny_sender(pool: &sqlx::PgPool, sender: Address) {
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
}
