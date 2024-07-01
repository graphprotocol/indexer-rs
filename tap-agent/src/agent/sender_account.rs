// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::num_bigint::ToBigInt;
use bigdecimal::ToPrimitive;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;
use tokio::task::JoinHandle;

use alloy_primitives::hex::ToHex;
use alloy_sol_types::Eip712Domain;
use anyhow::Result;
use ethereum_types::U256;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::subgraph_client::Query;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use ractor::{call, Actor, ActorProcessingErr, ActorRef, MessagingErr, SupervisionEvent};
use serde::Deserialize;
use sqlx::PgPool;
use tap_core::rav::SignedRAV;
use thegraph::types::Address;
use tracing::{error, Level};

use super::sender_allocation::{SenderAllocation, SenderAllocationArgs};
use crate::agent::sender_allocation::SenderAllocationMessage;
use crate::agent::sender_fee_tracker::SenderFeeTracker;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::escrow_adapter::EscrowAdapter,
};
type RavMap = HashMap<Address, u128>;
type Balance = U256;

#[derive(Debug)]
pub enum SenderAccountMessage {
    UpdateBalanceAndLastRavs(Balance, RavMap),
    UpdateAllocationIds(HashSet<Address>),
    NewAllocationId(Address),
    UpdateReceiptFees(Address, UnaggregatedReceipts),
    UpdateInvalidReceiptFees(Address, UnaggregatedReceipts),
    UpdateRav(SignedRAV),
    #[cfg(test)]
    GetSenderFeeTracker(ractor::RpcReplyPort<SenderFeeTracker>),
    #[cfg(test)]
    GetDeny(ractor::RpcReplyPort<bool>),
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
    pub config: &'static config::Config,
    pub pgpool: PgPool,
    pub sender_id: Address,
    pub escrow_accounts: Eventual<EscrowAccounts>,
    pub indexer_allocations: Eventual<HashSet<Address>>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub domain_separator: Eip712Domain,
    pub sender_aggregator_endpoint: String,
    pub allocation_ids: HashSet<Address>,
    pub prefix: Option<String>,

    pub retry_interval: Duration,
}
pub struct State {
    prefix: Option<String>,
    sender_fee_tracker: SenderFeeTracker,
    rav_tracker: SenderFeeTracker,
    invalid_receipts_tracker: SenderFeeTracker,
    allocation_ids: HashSet<Address>,
    _indexer_allocations_handle: PipeHandle,
    _escrow_account_monitor: PipeHandle,
    scheduled_rav_request: Option<JoinHandle<Result<(), MessagingErr<SenderAccountMessage>>>>,

    sender: Address,

    // Deny reasons
    denied: bool,
    sender_balance: U256,
    retry_interval: Duration,

    //Eventuals
    escrow_accounts: Eventual<EscrowAccounts>,

    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    domain_separator: Eip712Domain,
    config: &'static config::Config,
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
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
            config: self.config,
            pgpool: self.pgpool.clone(),
            allocation_id,
            sender: self.sender,
            escrow_accounts: self.escrow_accounts.clone(),
            escrow_subgraph: self.escrow_subgraph,
            escrow_adapter: self.escrow_adapter.clone(),
            domain_separator: self.domain_separator.clone(),
            sender_aggregator_endpoint: self.sender_aggregator_endpoint.clone(),
            sender_account_ref: sender_account_ref.clone(),
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

    async fn rav_requester_single(&mut self) -> Result<()> {
        let Some(allocation_id) = self.sender_fee_tracker.get_heaviest_allocation_id() else {
            anyhow::bail!(
                "Error while getting the heaviest allocation because \
                no unblocked allocation has enough unaggregated fees tracked"
            );
        };
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!(
                "Error while getting allocation actor {allocation_id} with most unaggregated fees"
            );
        };
        // we call and wait for the response so we don't process anymore update
        let Ok((fees, rav)) = call!(allocation, SenderAllocationMessage::TriggerRAVRequest) else {
            anyhow::bail!("Error while sending and waiting message for actor {allocation_id}");
        };

        // update rav tracker
        self.rav_tracker.update(
            allocation_id,
            rav.map_or(0, |rav| rav.message.valueAggregate),
        );

        // update sender fee tracker
        self.sender_fee_tracker.update(allocation_id, fees.value);
        Ok(())
    }

    fn deny_condition_reached(&self) -> bool {
        let pending_ravs = self.rav_tracker.get_total_fee();
        let unaggregated_fees = self.sender_fee_tracker.get_total_fee();
        let pending_fees_over_balance =
            pending_ravs + unaggregated_fees >= self.sender_balance.as_u128();
        let max_unaggregated_fees = self.config.tap.max_unnaggregated_fees_per_sender;
        let invalid_receipt_fees = self.invalid_receipts_tracker.get_total_fee();
        let total_fee_over_max_value =
            unaggregated_fees + invalid_receipt_fees >= max_unaggregated_fees;

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
            max_fee_per_sender = self.config.tap.max_unnaggregated_fees_per_sender,
            sender_balance = self.sender_balance.as_u128(),
            "Denying sender."
        );

        sqlx::query!(
            r#"
                    INSERT INTO scalar_tap_denylist (sender_address)
                    VALUES ($1) ON CONFLICT DO NOTHING
                "#,
            self.sender.encode_hex::<String>(),
        )
        .execute(&self.pgpool)
        .await
        .expect("Should not fail to insert into denylist");
        self.denied = true;
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    async fn remove_from_denylist(&mut self) {
        tracing::info!(
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_fee_per_sender = self.config.tap.max_unnaggregated_fees_per_sender,
            sender_balance = self.sender_balance.as_u128(),
            "Allowing sender."
        );
        sqlx::query!(
            r#"
                    DELETE FROM scalar_tap_denylist
                    WHERE sender_address = $1
                "#,
            self.sender.encode_hex::<String>(),
        )
        .execute(&self.pgpool)
        .await
        .expect("Should not fail to delete from denylist");
        self.denied = false;
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
            domain_separator,
            sender_aggregator_endpoint,
            allocation_ids,
            prefix,
            retry_interval,
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let myself_clone = myself.clone();
        let _indexer_allocations_handle =
            indexer_allocations
                .clone()
                .pipe_async(move |allocation_ids| {
                    let myself = myself_clone.clone();
                    async move {
                        // Update the allocation_ids
                        myself
                            .cast(SenderAccountMessage::UpdateAllocationIds(allocation_ids))
                            .unwrap_or_else(|e| {
                                error!("Error while updating allocation_ids: {:?}", e);
                            });
                    }
                });

        let myself_clone = myself.clone();
        let pgpool_clone = pgpool.clone();
        let _escrow_account_monitor = escrow_accounts.clone().pipe_async(move |escrow_account| {
            let myself = myself_clone.clone();
            let pgpool = pgpool_clone.clone();
            // get balance or default value for sender
            // this balance already takes into account thawing
            let balance = escrow_account
                .get_balance_for_sender(&sender_id)
                .unwrap_or_default();

            #[derive(Deserialize)]
            struct Transaction {
                #[serde(rename = "allocationID")]
                allocation_id: String,
            }

            #[derive(Deserialize)]
            struct Response {
                transactions: Vec<Transaction>,
            }

            async move {
                let last_non_final_ravs = sqlx::query!(
                    r#"
                            SELECT allocation_id, value_aggregate
                            FROM scalar_tap_ravs
                            WHERE sender_address = $1 AND last AND NOT final;
                        "#,
                    sender_id.encode_hex::<String>(),
                )
                .fetch_all(&pgpool)
                .await
                .expect("Should not fail to fetch from scalar_tap_ravs");

                let query = r#"
                    query transactions(
                        $unfinalizedRavsAllocationIds: [String!]!
                        $sender: String!
                    ) {
                        transactions(
                            where: {
                                type: "redeem"
                                allocationID_in: $unfinalizedRavsAllocationIds
                                sender_: { id: $sender }
                            }
                        ) {
                            allocationID
                        }
                    }
                "#;

                // get a list from the subgraph of which subgraphs were already redeemed and were not marked as final
                let redeemed_ravs_allocation_ids = match escrow_subgraph
                    .query::<Response>(Query::new_with_variables(
                        query,
                        [
                            (
                                "unfinalizedRavsAllocationIds",
                                last_non_final_ravs
                                    .iter()
                                    .map(|rav| rav.allocation_id.to_string())
                                    .collect::<Vec<_>>()
                                    .into(),
                            ),
                            ("sender", format!("{:x?}", sender_id).into()),
                        ],
                    ))
                    .await
                {
                    Ok(Ok(response)) => response
                        .transactions
                        .into_iter()
                        .map(|tx| tx.allocation_id)
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

        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone(), sender_id);

        // Get deny status from the scalar_tap_denylist table
        let denied = sqlx::query!(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM scalar_tap_denylist
                    WHERE sender_address = $1
                ) as denied
            "#,
            sender_id.encode_hex::<String>(),
        )
        .fetch_one(&pgpool)
        .await?
        .denied
        .expect("Deny status cannot be null");

        let sender_balance = escrow_accounts
            .value()
            .await
            .expect("should be able to get escrow accounts")
            .get_balance_for_sender(&sender_id)
            .unwrap_or_default();

        let state = State {
            sender_fee_tracker: SenderFeeTracker::default(),
            rav_tracker: SenderFeeTracker::default(),
            invalid_receipts_tracker: SenderFeeTracker::default(),
            allocation_ids: allocation_ids.clone(),
            _indexer_allocations_handle,
            _escrow_account_monitor,
            prefix,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            domain_separator,
            sender_aggregator_endpoint,
            config,
            pgpool,
            sender: sender_id,
            denied,
            sender_balance,
            retry_interval,
            scheduled_rav_request: None,
        };

        for allocation_id in &allocation_ids {
            // Create a sender allocation for each allocation
            state
                .create_sender_allocation(myself.clone(), *allocation_id)
                .await?;
        }

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
                state
                    .rav_tracker
                    .update(rav.message.allocationId, rav.message.valueAggregate);
                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }
            }
            SenderAccountMessage::UpdateInvalidReceiptFees(allocation_id, unaggregated_fees) => {
                state
                    .invalid_receipts_tracker
                    .update(allocation_id, unaggregated_fees.value);

                // invalid receipts can't go down
                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }
            }
            SenderAccountMessage::UpdateReceiptFees(allocation_id, unaggregated_fees) => {
                // If we're here because of a new receipt, abort any scheduled UpdateReceiptFees
                if let Some(scheduled_rav_request) = state.scheduled_rav_request.take() {
                    scheduled_rav_request.abort();
                }

                state
                    .sender_fee_tracker
                    .update(allocation_id, unaggregated_fees.value);

                // Eagerly deny the sender (if needed), before the RAV request. To be sure not to
                // delay the denial because of the RAV request, which could take some time.

                let should_deny = !state.denied && state.deny_condition_reached();
                if should_deny {
                    state.add_to_denylist().await;
                }

                if state.sender_fee_tracker.get_total_fee()
                    >= state.config.tap.rav_request_trigger_value
                {
                    tracing::debug!(
                        total_fee = state.sender_fee_tracker.get_total_fee(),
                        trigger_value = state.config.tap.rav_request_trigger_value,
                        "Total fee greater than the trigger value. Triggering RAV request"
                    );
                    // In case we fail, we want our actor to keep running
                    if let Err(err) = state.rav_requester_single().await {
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
                                    unaggregated_fees,
                                )
                            }));
                    }
                    _ => {}
                }
            }
            SenderAccountMessage::UpdateAllocationIds(allocation_ids) => {
                // Create new sender allocations
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
                    }
                }

                // Remove sender allocations
                for allocation_id in state.allocation_ids.difference(&allocation_ids) {
                    if let Some(sender_handle) = ActorRef::<SenderAllocationMessage>::where_is(
                        state.format_sender_allocation(allocation_id),
                    ) {
                        tracing::trace!(%allocation_id, "SenderAccount shutting down SenderAllocation");
                        // we can not send a rav request to this allocation
                        // because it's gonna trigger the last rav
                        state.sender_fee_tracker.block_allocation_id(*allocation_id);
                        sender_handle.stop(None);
                    }
                }

                tracing::trace!(
                    old_ids= ?state.allocation_ids,
                    new_ids = ?allocation_ids,
                    "Updating allocation ids"
                );
                state.allocation_ids = allocation_ids;
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
                    state.rav_tracker.update(*allocation_id, 0);
                }

                for (allocation_id, value) in non_final_last_ravs {
                    state.rav_tracker.update(allocation_id, value);
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

                let tracker = &mut state.sender_fee_tracker;
                tracker.update(allocation_id, 0);
                // clean up hashset
                state
                    .sender_fee_tracker
                    .unblock_allocation_id(allocation_id);
                // rav tracker is not updated because it's still not redeemed
            }
            SupervisionEvent::ActorPanicked(cell, error) => {
                let sender_allocation = cell.get_name();
                tracing::warn!(
                    ?sender_allocation,
                    ?error,
                    "Actor SenderAllocation panicked. Restarting..."
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

#[cfg(test)]
pub mod tests {
    use super::{SenderAccount, SenderAccountArgs, SenderAccountMessage};
    use crate::agent::sender_accounts_manager::NewReceiptNotification;
    use crate::agent::sender_allocation::SenderAllocationMessage;
    use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
    use crate::config;
    use crate::tap::test_utils::{
        create_rav, store_rav_with_options, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER, SENDER,
        SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    };
    use alloy_primitives::hex::ToHex;
    use alloy_primitives::Address;
    use eventuals::{Eventual, EventualWriter};
    use indexer_common::escrow_accounts::EscrowAccounts;
    use indexer_common::prelude::{DeploymentDetails, SubgraphClient};
    use ractor::concurrency::JoinHandle;
    use ractor::{call, Actor, ActorProcessingErr, ActorRef, ActorStatus};
    use serde_json::json;
    use sqlx::PgPool;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // we implement the PartialEq and Eq traits for SenderAccountMessage to be able to compare
    impl Eq for SenderAccountMessage {}

    impl PartialEq for SenderAccountMessage {
        fn eq(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::UpdateAllocationIds(l0), Self::UpdateAllocationIds(r0)) => l0 == r0,
                (Self::UpdateReceiptFees(l0, l1), Self::UpdateReceiptFees(r0, r1)) => {
                    l0 == r0 && l1 == r1
                }
                (
                    Self::UpdateInvalidReceiptFees(l0, l1),
                    Self::UpdateInvalidReceiptFees(r0, r1),
                ) => l0 == r0 && l1 == r1,
                (Self::NewAllocationId(l0), Self::NewAllocationId(r0)) => l0 == r0,
                (a, b) => unimplemented!("PartialEq not implementated for {a:?} and {b:?}"),
            }
        }
    }

    pub static PREFIX_ID: AtomicU32 = AtomicU32::new(0);
    const DUMMY_URL: &str = "http://localhost:1234";
    const TRIGGER_VALUE: u128 = 500;
    const ESCROW_VALUE: u128 = 1000;

    async fn create_sender_account(
        pgpool: PgPool,
        initial_allocation: HashSet<Address>,
        rav_request_trigger_value: u128,
        max_unnaggregated_fees_per_sender: u128,
        escrow_subgraph_endpoint: &str,
    ) -> (
        ActorRef<SenderAccountMessage>,
        tokio::task::JoinHandle<()>,
        String,
        EventualWriter<EscrowAccounts>,
    ) {
        let config = Box::leak(Box::new(config::Config {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value,
                rav_request_timestamp_buffer_ms: 1,
                rav_request_timeout_secs: 5,
                max_unnaggregated_fees_per_sender,
                ..Default::default()
            },
            ..Default::default()
        }));

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint, None).unwrap(),
        )));
        let (mut writer, escrow_accounts_eventual) = Eventual::new();

        writer.write(EscrowAccounts::new(
            HashMap::from([(SENDER.1, ESCROW_VALUE.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let prefix = format!(
            "test-{}",
            PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );

        let args = SenderAccountArgs {
            config,
            pgpool,
            sender_id: SENDER.1,
            escrow_accounts: escrow_accounts_eventual,
            indexer_allocations: Eventual::from_value(initial_allocation),
            escrow_subgraph,
            domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint: DUMMY_URL.to_string(),
            allocation_ids: HashSet::new(),
            prefix: Some(prefix.clone()),
            retry_interval: Duration::from_millis(10),
        };

        let (sender, handle) = SenderAccount::spawn(Some(prefix.clone()), SenderAccount, args)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        (sender, handle, prefix, writer)
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_allocation_ids(pgpool: PgPool) {
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            HashSet::new(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        // we expect it to create a sender allocation
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(
                vec![*ALLOCATION_ID_0].into_iter().collect(),
            ))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        // verify if create sender account
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_some());

        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_none());

        // safely stop the manager
        sender_account.stop_and_wait(None, None).await.unwrap();

        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_new_allocation_id(pgpool: PgPool) {
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            HashSet::new(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        // we expect it to create a sender allocation
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(*ALLOCATION_ID_0))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        // verify if create sender account
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_some());

        // nothing should change because we already created
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(
                vec![*ALLOCATION_ID_0].into_iter().collect(),
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;

        // try to delete sender allocation_id
        sender_account
            .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
        assert!(actor_ref.is_none());

        // safely stop the manager
        sender_account.stop_and_wait(None, None).await.unwrap();

        handle.await.unwrap();
    }

    pub struct MockSenderAllocation {
        triggered_rav_request: Arc<AtomicU32>,
        next_rav_value: Arc<Mutex<u128>>,
        receipts: Arc<Mutex<Vec<NewReceiptNotification>>>,
    }

    impl MockSenderAllocation {
        pub fn new_with_triggered_rav_request() -> (Self, Arc<AtomicU32>) {
            let triggered_rav_request = Arc::new(AtomicU32::new(0));
            (
                Self {
                    triggered_rav_request: triggered_rav_request.clone(),
                    receipts: Arc::new(Mutex::new(Vec::new())),
                    next_rav_value: Arc::new(Mutex::new(0)),
                },
                triggered_rav_request,
            )
        }

        pub fn new_with_next_rav_value() -> (Self, Arc<Mutex<u128>>) {
            let next_rav_value = Arc::new(Mutex::new(0));
            (
                Self {
                    triggered_rav_request: Arc::new(AtomicU32::new(0)),
                    receipts: Arc::new(Mutex::new(Vec::new())),
                    next_rav_value: next_rav_value.clone(),
                },
                next_rav_value,
            )
        }

        pub fn new_with_receipts() -> (Self, Arc<Mutex<Vec<NewReceiptNotification>>>) {
            let receipts = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    triggered_rav_request: Arc::new(AtomicU32::new(0)),
                    receipts: receipts.clone(),
                    next_rav_value: Arc::new(Mutex::new(0)),
                },
                receipts,
            )
        }
    }

    #[async_trait::async_trait]
    impl Actor for MockSenderAllocation {
        type Msg = SenderAllocationMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _allocation_ids: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            match message {
                SenderAllocationMessage::TriggerRAVRequest(reply) => {
                    self.triggered_rav_request
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let signed_rav = create_rav(
                        *ALLOCATION_ID_0,
                        SIGNER.0.clone(),
                        4,
                        *self.next_rav_value.lock().unwrap(),
                    );
                    reply.send((UnaggregatedReceipts::default(), Some(signed_rav)))?;
                }
                SenderAllocationMessage::NewReceipt(receipt) => {
                    self.receipts.lock().unwrap().push(receipt);
                }
                _ => {}
            }
            Ok(())
        }
    }

    async fn create_mock_sender_allocation(
        prefix: String,
        sender: Address,
        allocation: Address,
    ) -> (
        Arc<AtomicU32>,
        ActorRef<SenderAllocationMessage>,
        JoinHandle<()>,
    ) {
        let (mock_sender_allocation, triggered_rav_request) =
            MockSenderAllocation::new_with_triggered_rav_request();

        let name = format!("{}:{}:{}", prefix, sender, allocation);
        let (sender_account, join_handle) =
            MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
                .await
                .unwrap();
        (triggered_rav_request, sender_account, join_handle)
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_receipt_fees_no_rav(pgpool: PgPool) {
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            HashSet::new(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        let (triggered_rav_request, allocation, allocation_handle) =
            create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0).await;

        // create a fake sender allocation
        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                UnaggregatedReceipts {
                    value: TRIGGER_VALUE - 1,
                    last_id: 10,
                },
            ))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        assert_eq!(
            triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_receipt_fees_trigger_rav(pgpool: PgPool) {
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            HashSet::new(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        let (triggered_rav_request, allocation, allocation_handle) =
            create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0).await;

        // create a fake sender allocation
        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                UnaggregatedReceipts {
                    value: TRIGGER_VALUE,
                    last_id: 10,
                },
            ))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        assert_eq!(
            triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_remove_sender_account(pgpool: PgPool) {
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            vec![*ALLOCATION_ID_0].into_iter().collect(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        // check if allocation exists
        let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
        let Some(sender_allocation) =
            ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone())
        else {
            panic!("Sender allocation was not created");
        };

        // stop
        sender_account.stop_and_wait(None, None).await.unwrap();

        // check if sender_account is stopped
        assert_eq!(sender_account.get_status(), ActorStatus::Stopped);

        tokio::time::sleep(Duration::from_millis(10)).await;

        // check if sender_allocation is also stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        handle.await.unwrap();
    }

    /// Test that the deny status is correctly loaded from the DB at the start of the actor
    #[sqlx::test(migrations = "../migrations")]
    async fn test_init_deny(pgpool: PgPool) {
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            SENDER.1.encode_hex::<String>(),
        )
        .execute(&pgpool)
        .await
        .expect("Should not fail to insert into denylist");

        // make sure there's a reason to keep denied
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        let (sender_account, _handle, _, _) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            TRIGGER_VALUE,
            TRIGGER_VALUE,
            DUMMY_URL,
        )
        .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_retry_unaggregated_fees(pgpool: PgPool) {
        // we set to zero to block the sender, no matter the fee
        let max_unaggregated_fees_per_sender: u128 = 0;

        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool,
            HashSet::new(),
            TRIGGER_VALUE,
            max_unaggregated_fees_per_sender,
            DUMMY_URL,
        )
        .await;

        let (triggered_rav_request, allocation, allocation_handle) =
            create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0).await;

        assert_eq!(
            triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        sender_account
            .cast(SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                UnaggregatedReceipts {
                    value: TRIGGER_VALUE,
                    last_id: 11,
                },
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let retry_value = triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst);
        assert!(retry_value > 1, "It didn't retry more than once");

        tokio::time::sleep(Duration::from_millis(30)).await;

        let new_value = triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst);
        assert!(new_value > retry_value, "It didn't retry anymore");

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_deny_allow(pgpool: PgPool) {
        async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
            call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
        }

        let max_unaggregated_fees_per_sender: u128 = 1000;

        // Making sure no RAV is gonna be triggered during the test
        let (sender_account, handle, _, _) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            u128::MAX,
            max_unaggregated_fees_per_sender,
            DUMMY_URL,
        )
        .await;

        macro_rules! update_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        *ALLOCATION_ID_0,
                        UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                        },
                    ))
                    .unwrap();

                tokio::time::sleep(Duration::from_millis(10)).await;
            };
        }

        macro_rules! update_invalid_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                        *ALLOCATION_ID_0,
                        UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                        },
                    ))
                    .unwrap();

                tokio::time::sleep(Duration::from_millis(10)).await;
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
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_initialization_with_pending_ravs_over_the_limit(pgpool: PgPool) {
        // add last non-final ravs
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        let (sender_account, handle, _, _) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            TRIGGER_VALUE,
            u128::MAX,
            DUMMY_URL,
        )
        .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny);

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_unaggregated_fees_over_balance(pgpool: PgPool) {
        // add last non-final ravs
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        // other rav final, should not be taken into account
        let signed_rav = create_rav(*ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, true)
            .await
            .unwrap();

        let trigger_rav_request = ESCROW_VALUE * 2;

        // initialize with no trigger value and no max receipt deny
        let (sender_account, handle, prefix, _) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            trigger_rav_request,
            u128::MAX,
            DUMMY_URL,
        )
        .await;

        let (mock_sender_allocation, next_rav_value) =
            MockSenderAllocation::new_with_next_rav_value();

        let name = format!("{}:{}:{}", prefix, SENDER.1, *ALLOCATION_ID_0);
        let (allocation, allocation_handle) =
            MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
                .await
                .unwrap();

        async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
            call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
        }

        macro_rules! update_receipt_fees {
            ($value:expr) => {
                sender_account
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        *ALLOCATION_ID_0,
                        UnaggregatedReceipts {
                            value: $value,
                            last_id: 11,
                        },
                    ))
                    .unwrap();

                tokio::time::sleep(Duration::from_millis(10)).await;
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
        *next_rav_value.lock().unwrap() = trigger_rav_request;
        update_receipt_fees!(trigger_rav_request);

        // receipt fees should already be 0, but we are setting to 0 again
        update_receipt_fees!(0);

        // should stay denied because the value was transfered to rav
        let deny = get_deny_status(&sender_account).await;
        assert!(deny);

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_pending_rav_already_redeemed_and_redeem(pgpool: PgPool) {
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "transactions": [
                            {"allocationID": *ALLOCATION_ID_0 }
                        ]}}),
                    )),
            )
            .await;

        // redeemed
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        let signed_rav = create_rav(*ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE - 1);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        let (sender_account, handle, _, mut escrow_writer) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            TRIGGER_VALUE,
            u128::MAX,
            &mock_server.uri(),
        )
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
                            {"allocationID": *ALLOCATION_ID_0 },
                            {"allocationID": *ALLOCATION_ID_1 }
                        ]}}),
                    )),
            )
            .await;
        // escrow_account updated
        escrow_writer.write(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        // wait the actor react to the messages
        tokio::time::sleep(Duration::from_millis(10)).await;

        // should still be active with a 1 escrow available

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should keep unblocked");

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_thawing_deposit_process(pgpool: PgPool) {
        // add last non-final ravs
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
        store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
            .await
            .unwrap();

        let (sender_account, handle, _, mut escrow_writer) = create_sender_account(
            pgpool.clone(),
            HashSet::new(),
            TRIGGER_VALUE,
            u128::MAX,
            DUMMY_URL,
        )
        .await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should start unblocked");

        // update the escrow to a lower value
        escrow_writer.write(EscrowAccounts::new(
            HashMap::from([(SENDER.1, (ESCROW_VALUE / 2).into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(deny, "should block the sender");

        // simulate deposit
        escrow_writer.write(EscrowAccounts::new(
            HashMap::from([(SENDER.1, (ESCROW_VALUE).into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await;

        let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
        assert!(!deny, "should unblock the sender");

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }
}
