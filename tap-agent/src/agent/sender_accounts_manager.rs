// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::{collections::HashMap, str::FromStr};

use crate::agent::sender_allocation::SenderAllocationMessage;
use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::prelude::{Allocation, SubgraphClient};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use thegraph::types::Address;
use tracing::{error, warn};

use super::sender_account::{SenderAccount, SenderAccountArgs, SenderAccountMessage};
use crate::config;

#[derive(Deserialize, Debug)]
pub struct NewReceiptNotification {
    pub id: u64,
    pub allocation_id: Address,
    pub signer_address: Address,
    pub timestamp_ns: u64,
    pub value: u128,
}

pub struct SenderAccountsManager;

pub enum SenderAccountsManagerMessage {
    UpdateSenderAccounts(HashSet<Address>),
    CreateSenderAccount(Address, HashSet<Address>),
}

pub struct SenderAccountsManagerArgs {
    pub config: &'static config::Cli,
    pub domain_separator: Eip712Domain,

    pub pgpool: PgPool,
    pub indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    pub escrow_accounts: Eventual<EscrowAccounts>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub sender_aggregator_endpoints: HashMap<Address, String>,
}

pub struct State {
    sender_ids: HashSet<Address>,
    new_receipts_watcher_handle: tokio::task::JoinHandle<()>,
    _eligible_allocations_senders_pipe: PipeHandle,

    config: &'static config::Cli,
    domain_separator: Eip712Domain,
    pgpool: PgPool,
    indexer_allocations: Eventual<HashSet<Address>>,
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    sender_aggregator_endpoints: HashMap<Address, String>,
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
            sender_aggregator_endpoints,
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let indexer_allocations = indexer_allocations.map(|allocations| async move {
            allocations.keys().cloned().collect::<HashSet<Address>>()
        });
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );
        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle =
            tokio::spawn(new_receipts_watcher(pglistener, escrow_accounts.clone()));
        let clone = myself.clone();
        let _eligible_allocations_senders_pipe =
            escrow_accounts.clone().pipe_async(move |escrow_accounts| {
                let myself = clone.clone();

                async move {
                    myself
                        .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
                            escrow_accounts.get_senders(),
                        ))
                        .unwrap_or_else(|e| {
                            error!("Error while updating sender_accounts: {:?}", e);
                        });
                }
            });

        let state = State {
            config,
            domain_separator,
            sender_ids: HashSet::new(),
            new_receipts_watcher_handle,
            _eligible_allocations_senders_pipe,
            pgpool,
            indexer_allocations,
            escrow_accounts,
            escrow_subgraph,
            sender_aggregator_endpoints,
        };
        let sender_allocation = state.get_pending_sender_allocation_id().await;
        for (sender_id, allocation_ids) in sender_allocation {
            myself.cast(SenderAccountsManagerMessage::CreateSenderAccount(
                sender_id,
                allocation_ids,
            ))?;
        }

        Ok(state)
    }
    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        // Abort the notification watcher on drop. Otherwise it may panic because the PgPool could
        // get dropped before. (Observed in tests)
        state.new_receipts_watcher_handle.abort();
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match msg {
            SenderAccountsManagerMessage::UpdateSenderAccounts(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids) {
                    myself.cast(SenderAccountsManagerMessage::CreateSenderAccount(
                        *sender,
                        HashSet::new(),
                    ))?;
                }

                // Remove sender accounts
                for sender in state.sender_ids.difference(&target_senders) {
                    if let Some(sender_handle) =
                        ActorRef::<SenderAccountMessage>::where_is(sender.to_string())
                    {
                        sender_handle.cast(SenderAccountMessage::RemoveSenderAccount)?;
                    }
                }

                state.sender_ids = target_senders;
            }
            SenderAccountsManagerMessage::CreateSenderAccount(sender_id, allocation_ids) => {
                let args = state.new_sender_account_args(&sender_id, allocation_ids)?;
                SenderAccount::spawn_linked(
                    Some(sender_id.to_string()),
                    SenderAccount,
                    args,
                    myself.get_cell(),
                )
                .await?;
            }
        }
        Ok(())
    }
}

impl State {
    async fn get_pending_sender_allocation_id(&self) -> HashMap<Address, HashSet<Address>> {
        let escrow_accounts_snapshot = self
            .escrow_accounts
            .value()
            .await
            .expect("Should get escrow accounts from Eventual");

        // Gather all outstanding receipts and unfinalized RAVs from the database.
        // Used to create SenderAccount instances for all senders that have unfinalized allocations
        // and try to finalize them if they have become ineligible.

        // First we accumulate all allocations for each sender. This is because we may have more
        // than one signer per sender in DB.
        let mut unfinalized_sender_allocations_map: HashMap<Address, HashSet<Address>> =
            HashMap::new();

        let receipts_signer_allocations_in_db = sqlx::query!(
            r#"
                SELECT DISTINCT 
                    signer_address,
                    (
                        SELECT ARRAY 
                        (
                            SELECT DISTINCT allocation_id
                            FROM scalar_tap_receipts
                            WHERE signer_address = top.signer_address
                        )
                    ) AS allocation_ids
                FROM scalar_tap_receipts AS top
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
            let sender_id = escrow_accounts_snapshot
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
            domain_separator: self.domain_separator.clone(),
            sender_aggregator_endpoint: self
                .sender_aggregator_endpoints
                .get(sender_id)
                .ok_or_else(|| {
                    anyhow!(
                        "No sender_aggregator_endpoint found for sender {}",
                        sender_id
                    )
                })?
                .clone(),
            allocation_ids,
            prefix: None,
        })
    }
}

/// Continuously listens for new receipt notifications from Postgres and forwards them to the
/// corresponding SenderAccount.
async fn new_receipts_watcher(
    mut pglistener: PgListener,
    escrow_accounts: Eventual<EscrowAccounts>,
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

        let sender_address = escrow_accounts
            .value()
            .await
            .expect("should be able to get escrow accounts")
            .get_sender_for_signer(&new_receipt_notification.signer_address);

        let sender_address = match sender_address {
            Ok(sender_address) => sender_address,
            Err(_) => {
                error!(
                    "No sender address found for receipt signer address {}. \
                        This should not happen.",
                    new_receipt_notification.signer_address
                );
                // TODO: save the receipt in the failed receipts table?
                continue;
            }
        };
        let allocation_id = &new_receipt_notification.allocation_id;

        if let Some(sender_allocation) = ActorRef::<SenderAllocationMessage>::where_is(format!(
            "{sender_address}:{allocation_id}"
        )) {
            if let Err(e) = sender_allocation.cast(SenderAllocationMessage::NewReceipt(
                new_receipt_notification,
            )) {
                error!(
                    "Error while forwarding new receipt notification to sender_allocation: {:?}",
                    e
                );
            }
        } else {
            warn!(
                "No sender_allocation_manager found for sender_address {} to process new \
                    receipt notification. This should not happen.",
                sender_address
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_create_sender_accounts_manager() {}

    #[test]
    fn test_create_with_pending_sender_allocations() {}

    #[test]
    fn test_update_sender_allocation() {}

    #[test]
    fn test_create_sender_account() {}
}
