// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::time::Duration;
use std::{collections::HashMap, str::FromStr};

use super::sender_account::{
    SenderAccount, SenderAccountArgs, SenderAccountConfig, SenderAccountMessage,
};
use crate::agent::sender_allocation::SenderAllocationMessage;
use crate::lazy_static;
use alloy::dyn_abi::Eip712Domain;
use alloy::primitives::Address;
use anyhow::Result;
use anyhow::{anyhow, bail};
use futures::{stream, StreamExt};
use indexer_allocation::Allocation;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_watcher::watch_pipe;
use prometheus::{register_counter_vec, CounterVec};
use ractor::concurrency::JoinHandle;
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SupervisionEvent};
use reqwest::Url;
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use tokio::select;
use tokio::sync::watch::{self, Receiver};
use tracing::{error, warn};

#[cfg(test)]
mod tests;

lazy_static! {
    static ref RECEIPTS_CREATED: CounterVec = register_counter_vec!(
        "tap_receipts_received_total",
        "Receipts received since start of the program.",
        &["sender", "allocation"]
    )
    .unwrap();
}

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

pub struct State {
    sender_ids: HashSet<Address>,
    new_receipts_watcher_handle: Option<tokio::task::JoinHandle<()>>,
    _eligible_allocations_senders_handle: JoinHandle<()>,

    config: &'static SenderAccountConfig,
    domain_separator: Eip712Domain,
    pgpool: PgPool,
    indexer_allocations: Receiver<HashSet<Address>>,
    escrow_accounts: Receiver<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,
    sender_aggregator_endpoints: HashMap<Address, Url>,
    prefix: Option<String>,
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

impl State {
    fn format_sender_account(&self, sender: &Address) -> String {
        let mut sender_allocation_id = String::new();
        if let Some(prefix) = &self.prefix {
            sender_allocation_id.push_str(prefix);
            sender_allocation_id.push(':');
        }
        sender_allocation_id.push_str(&format!("{}", sender));
        sender_allocation_id
    }

    async fn create_or_deny_sender(
        &self,
        supervisor: ActorCell,
        sender_id: Address,
        allocation_ids: HashSet<Address>,
    ) {
        if let Err(e) = self
            .create_sender_account(supervisor, sender_id, allocation_ids)
            .await
        {
            error!(
                "There was an error while starting the sender {}, denying it. Error: {:?}",
                sender_id, e
            );
            SenderAccount::deny_sender(&self.pgpool, sender_id).await;
        }
    }

    async fn create_sender_account(
        &self,
        supervisor: ActorCell,
        sender_id: Address,
        allocation_ids: HashSet<Address>,
    ) -> anyhow::Result<()> {
        let Ok(args) = self.new_sender_account_args(&sender_id, allocation_ids) else {
            warn!(
                "Sender {} is not on your [tap.sender_aggregator_endpoints] list. \
                        \
                        This means that you don't recognize this sender and don't want to \
                        provide queries for it.
                        \
                        If you do recognize and want to serve queries for it, \
                        add a new entry to the config [tap.sender_aggregator_endpoints]",
                sender_id
            );
            bail!(
                "No sender_aggregator_endpoints found for sender {}",
                sender_id
            );
        };
        SenderAccount::spawn_linked(
            Some(self.format_sender_account(&sender_id)),
            SenderAccount,
            args,
            supervisor,
        )
        .await?;
        Ok(())
    }

    async fn get_pending_sender_allocation_id(&self) -> HashMap<Address, HashSet<Address>> {
        // Gather all outstanding receipts and unfinalized RAVs from the database.
        // Used to create SenderAccount instances for all senders that have unfinalized allocations
        // and try to finalize them if they have become ineligible.

        // First we accumulate all allocations for each sender. This is because we may have more
        // than one signer per sender in DB.
        let mut unfinalized_sender_allocations_map: HashMap<Address, HashSet<Address>> =
            HashMap::new();

        let receipts_signer_allocations_in_db = sqlx::query!(
            r#"
                WITH grouped AS (
                    SELECT signer_address, allocation_id
                    FROM scalar_tap_receipts
                    GROUP BY signer_address, allocation_id
                )
                SELECT DISTINCT
                    signer_address,
                    (
                        SELECT ARRAY
                        (
                            SELECT DISTINCT allocation_id
                            FROM grouped
                            WHERE signer_address = top.signer_address
                        )
                    ) AS allocation_ids
                FROM grouped AS top
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch pending receipts from the database");

        for row in receipts_signer_allocations_in_db {
            let allocation_ids = row
                .allocation_ids
                .expect("all receipts should have an allocation_id")
                .iter()
                .map(|allocation_id| {
                    Address::from_str(allocation_id)
                        .expect("allocation_id should be a valid address")
                })
                .collect::<HashSet<Address>>();
            let signer_id = Address::from_str(&row.signer_address)
                .expect("signer_address should be a valid address");
            let sender_id = self
                .escrow_accounts
                .borrow()
                .get_sender_for_signer(&signer_id)
                .expect("should be able to get sender from signer");

            // Accumulate allocations for the sender
            unfinalized_sender_allocations_map
                .entry(sender_id)
                .or_default()
                .extend(allocation_ids);
        }

        let nonfinal_ravs_sender_allocations_in_db = sqlx::query!(
            r#"
                SELECT DISTINCT
                    sender_address,
                    (
                        SELECT ARRAY
                        (
                            SELECT DISTINCT allocation_id
                            FROM scalar_tap_ravs
                            WHERE sender_address = top.sender_address
                            AND NOT last
                        )
                    ) AS allocation_id
                FROM scalar_tap_ravs AS top
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch unfinalized RAVs from the database");

        for row in nonfinal_ravs_sender_allocations_in_db {
            let allocation_ids = row
                .allocation_id
                .expect("all RAVs should have an allocation_id")
                .iter()
                .map(|allocation_id| {
                    Address::from_str(allocation_id)
                        .expect("allocation_id should be a valid address")
                })
                .collect::<HashSet<Address>>();
            let sender_id = Address::from_str(&row.sender_address)
                .expect("sender_address should be a valid address");

            // Accumulate allocations for the sender
            unfinalized_sender_allocations_map
                .entry(sender_id)
                .or_default()
                .extend(allocation_ids);
        }
        unfinalized_sender_allocations_map
    }
    fn new_sender_account_args(
        &self,
        sender_id: &Address,
        allocation_ids: HashSet<Address>,
    ) -> Result<SenderAccountArgs> {
        Ok(SenderAccountArgs {
            config: self.config,
            pgpool: self.pgpool.clone(),
            sender_id: *sender_id,
            escrow_accounts: self.escrow_accounts.clone(),
            indexer_allocations: self.indexer_allocations.clone(),
            escrow_subgraph: self.escrow_subgraph,
            network_subgraph: self.network_subgraph,
            domain_separator: self.domain_separator.clone(),
            sender_aggregator_endpoint: self
                .sender_aggregator_endpoints
                .get(sender_id)
                .ok_or(anyhow!(
                    "No sender_aggregator_endpoints found for sender {}",
                    sender_id
                ))?
                .clone(),
            allocation_ids,
            prefix: self.prefix.clone(),
            retry_interval: Duration::from_secs(30),
        })
    }
}

/// Continuously listens for new receipt notifications from Postgres and forwards them to the
/// corresponding SenderAccount.
async fn new_receipts_watcher(
    mut pglistener: PgListener,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<String>,
) {
    loop {
        // TODO: recover from errors or shutdown the whole program?
        let pg_notification = pglistener.recv().await.expect(
            "should be able to receive Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
        );
        let new_receipt_notification: NewReceiptNotification =
            serde_json::from_str(pg_notification.payload()).expect(
                "should be able to deserialize the Postgres Notify event payload as a \
                        NewReceiptNotification",
            );
        if let Err(e) = handle_notification(
            new_receipt_notification,
            escrow_accounts_rx.clone(),
            prefix.as_deref(),
        )
        .await
        {
            error!("{}", e);
        }
    }
}

async fn handle_notification(
    new_receipt_notification: NewReceiptNotification,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<&str>,
) -> Result<()> {
    tracing::trace!(
        notification = ?new_receipt_notification,
        "New receipt notification detected!"
    );

    let Ok(sender_address) = escrow_accounts_rx
        .borrow()
        .get_sender_for_signer(&new_receipt_notification.signer_address)
    else {
        // TODO: save the receipt in the failed receipts table?
        bail!(
            "No sender address found for receipt signer address {}. \
                    This should not happen.",
            new_receipt_notification.signer_address
        );
    };

    let allocation_id = &new_receipt_notification.allocation_id;
    let allocation_str = &allocation_id.to_string();

    let actor_name = format!(
        "{}{sender_address}:{allocation_id}",
        prefix
            .as_ref()
            .map_or(String::default(), |prefix| format!("{prefix}:"))
    );

    let Some(sender_allocation) = ActorRef::<SenderAllocationMessage>::where_is(actor_name) else {
        warn!(
            "No sender_allocation found for sender_address {}, allocation_id {} to process new \
                receipt notification. Starting a new sender_allocation.",
            sender_address, allocation_id
        );
        let sender_account_name = format!(
            "{}{sender_address}",
            prefix
                .as_ref()
                .map_or(String::default(), |prefix| format!("{prefix}:"))
        );

        let Some(sender_account) = ActorRef::<SenderAccountMessage>::where_is(sender_account_name)
        else {
            bail!(
                "No sender_account was found for address: {}.",
                sender_address
            );
        };
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(*allocation_id))
            .map_err(|e| {
                anyhow!(
                    "Error while sendeing new allocation id message to sender_account: {:?}",
                    e
                )
            })?;
        return Ok(());
    };

    sender_allocation
        .cast(SenderAllocationMessage::NewReceipt(
            new_receipt_notification,
        ))
        .map_err(|e| {
            anyhow::anyhow!(
                "Error while forwarding new receipt notification to sender_allocation: {:?}",
                e
            )
        })?;

    RECEIPTS_CREATED
        .with_label_values(&[&sender_address.to_string(), allocation_str])
        .inc();
    Ok(())
}
