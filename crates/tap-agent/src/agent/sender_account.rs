// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{
    dyn_abi::Eip712Domain,
    hex::ToHexExt,
    primitives::{Address, U256},
};
use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
use futures::{stream, StreamExt};
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_query::{unfinalized_transactions, UnfinalizedTransactions};
use indexer_watcher::watch_pipe;
use jsonrpsee::http_client::HttpClientBuilder;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use reqwest::Url;
use sqlx::PgPool;
use state::State;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};
use tap_core::rav::SignedRAV;
use tokio::sync::watch::Receiver;
use tracing::{error, warn, Level};
use typed_builder::TypedBuilder;

use crate::{
    adaptative_concurrency::AdaptiveLimiter,
    agent::{
        metrics::{
            ESCROW_BALANCE, INVALID_RECEIPT_FEES, MAX_FEE_PER_SENDER, PENDING_RAV,
            RAV_REQUEST_TRIGGER_VALUE, SENDER_DENIED, SENDER_FEE_TRACKER, UNAGGREGATED_FEES,
        },
        sender_allocation::SenderAllocationMessage,
        unaggregated_receipts::UnaggregatedReceipts,
    },
    tracker::SenderFeeTracker,
};

mod state;
#[cfg(test)]
pub mod tests;

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

#[derive(TypedBuilder)]
pub struct SenderAccountArgs {
    config: &'static SenderAccountConfig,

    pgpool: PgPool,
    sender_id: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    indexer_allocations: Receiver<HashSet<Address>>,
    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,
    domain_separator: Eip712Domain,
    sender_aggregator_endpoint: Url,
    allocation_ids: HashSet<Address>,
    prefix: Option<String>,

    retry_interval: Duration,
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
    pub fn from_config(config: &indexer_config::Config) -> &'static Self {
        Box::leak(Box::new(Self {
            rav_request_buffer: config.tap.rav_request.timestamp_buffer_secs,
            rav_request_receipt_limit: config.tap.rav_request.max_receipts_per_request,
            indexer_address: config.indexer.indexer_address,
            escrow_polling_interval: config.subgraphs.escrow.config.syncing_interval_secs,
            max_amount_willing_to_lose_grt: config.tap.max_amount_willing_to_lose_grt.get_value(),
            trigger_value: config.tap.get_trigger_value(),
            rav_request_timeout: config.tap.rav_request.request_timeout_secs,
        }))
    }
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

        let state = State::builder()
            .prefix(prefix)
            .sender_fee_tracker(SenderFeeTracker::new(config.rav_request_buffer))
            .allocation_ids(allocation_ids.clone())
            ._indexer_allocations_handle(_indexer_allocations_handle)
            ._escrow_account_monitor(_escrow_account_monitor)
            .sender(sender_id)
            .denied(denied)
            .sender_balance(sender_balance)
            .retry_interval(retry_interval)
            .adaptive_limiter(AdaptiveLimiter::new(INITIAL_RAV_REQUEST_CONCURRENT, 1..50))
            .escrow_accounts(escrow_accounts)
            .escrow_subgraph(escrow_subgraph)
            .network_subgraph(network_subgraph)
            .domain_separator(domain_separator)
            .pgpool(pgpool)
            .sender_aggregator(sender_aggregator)
            .config(config)
            .build();

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
