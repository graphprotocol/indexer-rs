// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};

use super::sender_account::{SenderAccountConfig, SenderAccountMessage};
use alloy::{dyn_abi::Eip712Domain, primitives::Address};
use futures::{stream, StreamExt};
use indexer_allocation::Allocation;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_watcher::watch_pipe;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use receipt_watcher::new_receipts_watcher;
use reqwest::Url;
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use state::State;
use tokio::{
    select,
    sync::watch::{self, Receiver},
};
use tracing::error;

mod receipt_watcher;
mod state;
#[cfg(test)]
mod tests;

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct NewReceiptNotification {
    pub id: u64,
    pub allocation_id: Address,
    pub signer_address: Address,
    pub timestamp_ns: u64,
    pub value: u128,
}

pub struct SenderAccountsManager;

#[derive(Debug)]
pub enum SenderAccountsManagerMessage {
    UpdateSenderAccounts(HashSet<Address>),
}

pub struct SenderAccountsManagerArgs {
    pub config: &'static SenderAccountConfig,
    pub domain_separator: Eip712Domain,

    pub pgpool: PgPool,
    pub indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    pub escrow_accounts: Receiver<EscrowAccounts>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub network_subgraph: &'static SubgraphClient,
    pub sender_aggregator_endpoints: HashMap<Address, Url>,

    pub prefix: Option<String>,
}
#[async_trait::async_trait]
impl Actor for SenderAccountsManager {
    type Msg = SenderAccountsManagerMessage;
    type State = State;
    type Arguments = SenderAccountsManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        SenderAccountsManagerArgs {
            config,
            domain_separator,
            indexer_allocations,
            pgpool,
            escrow_accounts,
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix,
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let (allocations_tx, allocations_rx) = watch::channel(HashSet::<Address>::new());
        watch_pipe(indexer_allocations.clone(), move |allocation_id| {
            let allocation_set = allocation_id.keys().cloned().collect::<HashSet<Address>>();
            allocations_tx
                .send(allocation_set)
                .expect("Failed to update indexer_allocations_set channel");
            async {}
        });
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );
        let myself_clone = myself.clone();
        let accounts_clone = escrow_accounts.clone();
        let _eligible_allocations_senders_handle =
            watch_pipe(accounts_clone, move |escrow_accounts| {
                let senders = escrow_accounts.get_senders();
                myself_clone
                    .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(senders))
                    .unwrap_or_else(|e| {
                        error!("Error while updating sender_accounts: {:?}", e);
                    });
                async {}
            });

        let mut state = State {
            config,
            domain_separator,
            sender_ids: HashSet::new(),
            new_receipts_watcher_handle: None,
            _eligible_allocations_senders_handle,
            pgpool,
            indexer_allocations: allocations_rx,
            escrow_accounts: escrow_accounts.clone(),
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix: prefix.clone(),
        };
        let sender_allocation = select! {
            sender_allocation = state.get_pending_sender_allocation_id() => sender_allocation,
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                panic!("Timeout while getting pending sender allocation ids");
            }
        };

        state.sender_ids.extend(sender_allocation.keys());

        stream::iter(sender_allocation)
            .map(|(sender_id, allocation_ids)| {
                state.create_or_deny_sender(myself.get_cell(), sender_id, allocation_ids)
            })
            .buffer_unordered(10) // Limit concurrency to 10 senders at a time
            .collect::<Vec<()>>()
            .await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        // after starting all senders
        state.new_receipts_watcher_handle = Some(tokio::spawn(new_receipts_watcher(
            pglistener,
            escrow_accounts,
            prefix,
        )));

        tracing::info!("SenderAccountManager created!");
        Ok(state)
    }
    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        // Abort the notification watcher on drop. Otherwise it may panic because the PgPool could
        // get dropped before. (Observed in tests)
        if let Some(handle) = &state.new_receipts_watcher_handle {
            handle.abort();
        }
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        tracing::trace!(
            message = ?msg,
            "New SenderAccountManager message"
        );

        match msg {
            SenderAccountsManagerMessage::UpdateSenderAccounts(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids) {
                    state
                        .create_or_deny_sender(myself.get_cell(), *sender, HashSet::new())
                        .await;
                }

                // Remove sender accounts
                for sender in state.sender_ids.difference(&target_senders) {
                    if let Some(sender_handle) = ActorRef::<SenderAccountMessage>::where_is(
                        state.format_sender_account(sender),
                    ) {
                        sender_handle.stop(None);
                    }
                }

                state.sender_ids = target_senders;
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
        match message {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let sender_id = cell.get_name();
                tracing::info!(?sender_id, ?reason, "Actor SenderAccount was terminated")
            }
            SupervisionEvent::ActorFailed(cell, error) => {
                let sender_id = cell.get_name();
                tracing::warn!(
                    ?sender_id,
                    ?error,
                    "Actor SenderAccount failed. Restarting..."
                );
                let Some(sender_id) = cell.get_name() else {
                    tracing::error!("SenderAllocation doesn't have a name");
                    return Ok(());
                };
                let Some(sender_id) = sender_id.split(':').last() else {
                    tracing::error!(%sender_id, "Could not extract sender_id from name");
                    return Ok(());
                };
                let Ok(sender_id) = Address::parse_checksummed(sender_id, None) else {
                    tracing::error!(%sender_id, "Could not convert sender_id to Address");
                    return Ok(());
                };

                let mut sender_allocation = select! {
                    sender_allocation = state.get_pending_sender_allocation_id() => sender_allocation,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                        tracing::error!("Timeout while getting pending sender allocation ids");
                        return Ok(());
                    }
                };

                let allocations = sender_allocation
                    .remove(&sender_id)
                    .unwrap_or(HashSet::new());

                state
                    .create_or_deny_sender(myself.get_cell(), sender_id, allocations)
                    .await;
            }
            _ => {}
        }
        Ok(())
    }
}
