// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use crate::agent::sender_account::{SenderAccount, SenderAccountArgs, SenderAccountConfig};
use alloy::{dyn_abi::Eip712Domain, primitives::Address};
use anyhow::{anyhow, bail, Result};
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use ractor::{concurrency::JoinHandle, Actor, ActorCell};
use reqwest::Url;
use sqlx::PgPool;
use tokio::sync::watch::Receiver;
use tracing::{error, warn};
use typed_builder::TypedBuilder;

#[derive(TypedBuilder)]
pub struct State {
    #[builder(default)]
    pub sender_ids: HashSet<Address>,

    #[builder(default)]
    pub new_receipts_watcher_handle: Option<tokio::task::JoinHandle<()>>,
    _eligible_allocations_senders_handle: JoinHandle<()>,

    config: &'static SenderAccountConfig,
    domain_separator: Eip712Domain,
    pgpool: PgPool,
    indexer_allocations: Receiver<HashSet<Address>>,
    escrow_accounts: Receiver<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,
    sender_aggregator_endpoints: HashMap<Address, Url>,
    #[builder(default)]
    prefix: Option<String>,
}

impl State {
    pub(super) fn format_sender_account(&self, sender: &Address) -> String {
        let mut sender_allocation_id = String::new();
        if let Some(prefix) = &self.prefix {
            sender_allocation_id.push_str(prefix);
            sender_allocation_id.push(':');
        }
        sender_allocation_id.push_str(&format!("{}", sender));
        sender_allocation_id
    }

    pub(super) async fn create_or_deny_sender(
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

    pub(super) async fn create_sender_account(
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

    pub(super) async fn get_pending_sender_allocation_id(
        &self,
    ) -> HashMap<Address, HashSet<Address>> {
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
        Ok(SenderAccountArgs::builder()
            .config(self.config)
            .pgpool(self.pgpool.clone())
            .sender_id(*sender_id)
            .escrow_accounts(self.escrow_accounts.clone())
            .indexer_allocations(self.indexer_allocations.clone())
            .escrow_subgraph(self.escrow_subgraph)
            .network_subgraph(self.network_subgraph)
            .domain_separator(self.domain_separator.clone())
            .sender_aggregator_endpoint(
                self.sender_aggregator_endpoints
                    .get(sender_id)
                    .ok_or(anyhow!(
                        "No sender_aggregator_endpoints found for sender {}",
                        sender_id
                    ))?
                    .clone(),
            )
            .allocation_ids(allocation_ids)
            .prefix(self.prefix.clone())
            .retry_interval(Duration::from_secs(30))
            .build())
    }
}
