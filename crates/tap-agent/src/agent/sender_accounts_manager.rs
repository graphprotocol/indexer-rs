// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    str::FromStr,
    sync::LazyLock,
    time::Duration,
};

use anyhow::{anyhow, bail};
use futures::{stream, StreamExt};
use indexer_allocation::Allocation;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_watcher::{map_watcher, watch_pipe};
use prometheus::{register_counter_vec, CounterVec};
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SupervisionEvent};
use reqwest::Url;
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address, sol_types::Eip712Domain},
    AllocationId as AllocationIdCore, CollectionId,
};
use tokio::{select, sync::watch::Receiver};

use super::sender_account::{
    SenderAccount, SenderAccountArgs, SenderAccountConfig, SenderAccountMessage,
};
use crate::agent::sender_allocation::SenderAllocationMessage;

static RECEIPTS_CREATED: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_receipts_received_total",
        "Receipts received since start of the program.",
        &["sender", "allocation"]
    )
    .unwrap()
});

/// Notification received by pgnotify
///
/// This contains a list of properties that are sent by postgres when a receipt is inserted
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct NewReceiptNotification {
    /// id inside the table
    pub id: u64,
    /// address of the allocation
    pub allocation_id: Address,
    /// address of wallet that signed this receipt
    pub signer_address: Address,
    /// timestamp of the receipt
    pub timestamp_ns: u64,
    /// value of the receipt
    pub value: u128,
}

/// Manager Actor
#[derive(Debug, Clone)]
pub struct SenderAccountsManager;

/// Wrapped AllocationId with two possible variants
///
/// This is used by children actors to define what kind of
/// SenderAllocation must be created to handle the correct
/// Rav and Receipt types
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AllocationId {
    /// Legacy allocation using AllocationId from thegraph-core
    Legacy(AllocationIdCore),
    /// New Subgraph DataService allocation using CollectionId
    Horizon(CollectionId),
}

impl AllocationId {
    /// Get a hex string representation for database queries
    pub fn to_hex(&self) -> String {
        match self {
            AllocationId::Legacy(allocation_id) => allocation_id.to_string(),
            AllocationId::Horizon(collection_id) => collection_id.to_string(),
        }
    }

    /// Get the underlying Address for Legacy allocations
    pub fn as_address(&self) -> Option<Address> {
        match self {
            AllocationId::Legacy(allocation_id) => Some(**allocation_id),
            AllocationId::Horizon(_) => None,
        }
    }

    /// Get an Address representation for both allocation types
    pub fn address(&self) -> Address {
        match self {
            AllocationId::Legacy(allocation_id) => **allocation_id,
            AllocationId::Horizon(collection_id) => collection_id.as_address(),
        }
    }
}

impl Display for AllocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocationId::Legacy(allocation_id) => write!(f, "{}", allocation_id),
            AllocationId::Horizon(collection_id) => write!(f, "{}", collection_id),
        }
    }
}

/// Type used in [SenderAccountsManager] and [SenderAccount] to route the correct escrow queries
/// and to use the correct set of tables
#[derive(Clone, Copy)]
pub enum SenderType {
    /// SenderAccounts that are found in Escrow Subgraph v1 (Legacy)
    Legacy,
    /// SenderAccounts that are found in Tap Collector v2 (Horizon)
    Horizon,
}

/// Enum containing all types of messages that a [SenderAccountsManager] can receive
#[derive(Debug)]
#[cfg_attr(any(test, feature = "test"), derive(Clone))]
pub enum SenderAccountsManagerMessage {
    /// Spawn and Stop [SenderAccount]s that were added or removed
    /// in comparison with it current state and updates the state
    ///
    /// This tracks only v1 accounts
    UpdateSenderAccountsV1(HashSet<Address>),

    /// Spawn and Stop [SenderAccount]s that were added or removed
    /// in comparison with it current state and updates the state
    ///
    /// This tracks only v2 accounts
    UpdateSenderAccountsV2(HashSet<Address>),
}

/// Arguments received in startup while spawing [SenderAccount] actor
pub struct SenderAccountsManagerArgs {
    /// Config forwarded to [SenderAccount]
    pub config: &'static SenderAccountConfig,
    /// Domain separator used for tap
    pub domain_separator: Eip712Domain,

    /// Database connection
    pub pgpool: PgPool,
    /// Watcher that returns a map of open and recently closed allocation ids
    pub indexer_allocations: Receiver<HashMap<Address, Allocation>>,
    /// Watcher containing the escrow accounts for v1
    pub escrow_accounts_v1: Receiver<EscrowAccounts>,
    /// Watcher containing the escrow accounts for v2
    pub escrow_accounts_v2: Receiver<EscrowAccounts>,
    /// SubgraphClient of the escrow subgraph
    pub escrow_subgraph: &'static SubgraphClient,
    /// SubgraphClient of the network subgraph
    pub network_subgraph: &'static SubgraphClient,
    /// Map containing all endpoints for senders provided in the config
    pub sender_aggregator_endpoints: HashMap<Address, Url>,

    /// Prefix used to bypass limitations of global actor registry (used for tests)
    pub prefix: Option<String>,
}

/// State for [SenderAccountsManager] actor
///
/// This is a separate instance that makes it easier to have mutable
/// reference, for more information check ractor library
pub struct State {
    sender_ids_v1: HashSet<Address>,
    sender_ids_v2: HashSet<Address>,
    new_receipts_watcher_handle_v1: Option<tokio::task::JoinHandle<()>>,
    new_receipts_watcher_handle_v2: Option<tokio::task::JoinHandle<()>>,

    config: &'static SenderAccountConfig,
    domain_separator: Eip712Domain,
    pgpool: PgPool,
    indexer_allocations: Receiver<HashSet<AllocationId>>,
    /// Watcher containing the escrow accounts for v1
    escrow_accounts_v1: Receiver<EscrowAccounts>,
    /// Watcher containing the escrow accounts for v2
    escrow_accounts_v2: Receiver<EscrowAccounts>,
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

    /// This is called in the [ractor::Actor::spawn] method and is used
    /// to process the [SenderAccountsManagerArgs] with a reference to the current
    /// actor
    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        SenderAccountsManagerArgs {
            config,
            domain_separator,
            indexer_allocations,
            pgpool,
            escrow_accounts_v1,
            escrow_accounts_v2,
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix,
        }: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let indexer_allocations = map_watcher(indexer_allocations, move |allocation_id| {
            allocation_id
                .keys()
                .cloned()
                // TODO: map based on the allocation type returned by the subgraph
                .map(|addr| AllocationId::Legacy(AllocationIdCore::from(addr)))
                .collect::<HashSet<_>>()
        });
        // we need two connections because each one will listen to different notify events
        let pglistener_v1 = PgListener::connect_with(&pgpool.clone()).await.unwrap();

        // Extra safety, we don't want to have a listener if horizon is not enabled
        let pglistener_v2 = if config.horizon_enabled {
            Some(PgListener::connect_with(&pgpool.clone()).await.unwrap())
        } else {
            None
        };

        let myself_clone = myself.clone();
        let accounts_clone = escrow_accounts_v1.clone();
        watch_pipe(accounts_clone, move |escrow_accounts| {
            let senders = escrow_accounts.get_senders();
            myself_clone
                .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV1(
                    senders,
                ))
                .unwrap_or_else(|e| {
                    tracing::error!("Error while updating sender_accounts v1: {:?}", e);
                });
            async {}
        });

        // Extra safety, we don't want to have a
        // escrow account listener if horizon is not enabled
        if config.horizon_enabled {
            let myself_clone = myself.clone();
            let _escrow_accounts_v2 = escrow_accounts_v2.clone();
            watch_pipe(_escrow_accounts_v2, move |escrow_accounts| {
                let senders = escrow_accounts.get_senders();
                myself_clone
                    .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV2(
                        senders,
                    ))
                    .unwrap_or_else(|e| {
                        tracing::error!("Error while updating sender_accounts v2: {:?}", e);
                    });
                async {}
            });
        }

        let mut state = State {
            config,
            domain_separator,
            sender_ids_v1: HashSet::new(),
            sender_ids_v2: HashSet::new(),
            new_receipts_watcher_handle_v1: None,
            new_receipts_watcher_handle_v2: None,
            pgpool: pgpool.clone(),
            indexer_allocations,
            escrow_accounts_v1: escrow_accounts_v1.clone(),
            escrow_accounts_v2: escrow_accounts_v2.clone(),
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix: prefix.clone(),
        };
        // v1
        let sender_allocation_v1 = select! {
            sender_allocation = state.get_pending_sender_allocation_id_v1() => sender_allocation,
            _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                panic!("Timeout while getting pending sender allocation ids");
            }
        };
        state.sender_ids_v1.extend(sender_allocation_v1.keys());
        stream::iter(sender_allocation_v1)
            .map(|(sender_id, allocation_ids)| {
                state.create_or_deny_sender(
                    myself.get_cell(),
                    sender_id,
                    allocation_ids,
                    SenderType::Legacy,
                )
            })
            .buffer_unordered(10) // Limit concurrency to 10 senders at a time
            .collect::<Vec<()>>()
            .await;

        // v2
        let sender_allocation_v2 = if state.config.horizon_enabled {
            select! {
                sender_allocation = state.get_pending_sender_allocation_id_v2() => sender_allocation,
                _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                    panic!("Timeout while getting pending sender allocation ids");
                }
            }
        } else {
            HashMap::new()
        };

        state.sender_ids_v2.extend(sender_allocation_v2.keys());
        stream::iter(sender_allocation_v2)
            .map(|(sender_id, allocation_ids)| {
                state.create_or_deny_sender(
                    myself.get_cell(),
                    sender_id,
                    allocation_ids,
                    SenderType::Horizon,
                )
            })
            .buffer_unordered(10) // Limit concurrency to 10 senders at a time
            .collect::<Vec<()>>()
            .await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        // after starting all senders
        state.new_receipts_watcher_handle_v1 = Some(tokio::spawn(
            new_receipts_watcher()
                .sender_type(SenderType::Legacy)
                .actor_cell(myself.get_cell())
                .pglistener(pglistener_v1)
                .escrow_accounts_rx(escrow_accounts_v1)
                .maybe_prefix(prefix.clone())
                .call(),
        ));

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        // after starting all senders
        state.new_receipts_watcher_handle_v2 = None;

        // Extra safety, we don't want to have a listener if horizon is not enabled
        if let Some(listener_v2) = pglistener_v2 {
            state.new_receipts_watcher_handle_v2 = Some(tokio::spawn(
                new_receipts_watcher()
                    .actor_cell(myself.get_cell())
                    .pglistener(listener_v2)
                    .escrow_accounts_rx(escrow_accounts_v2)
                    .sender_type(SenderType::Horizon)
                    .maybe_prefix(prefix)
                    .call(),
            ));
        };

        tracing::info!("SenderAccountManager created!");
        Ok(state)
    }

    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Abort the notification watcher on drop. Otherwise it may panic because the PgPool could
        // get dropped before. (Observed in tests)
        if let Some(handle) = &state.new_receipts_watcher_handle_v1 {
            handle.abort();
        }

        if let Some(handle) = &state.new_receipts_watcher_handle_v2 {
            handle.abort();
        }

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::trace!(
            message = ?msg,
            "New SenderAccountManager message"
        );

        match msg {
            SenderAccountsManagerMessage::UpdateSenderAccountsV1(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids_v1) {
                    state
                        .create_or_deny_sender(
                            myself.get_cell(),
                            *sender,
                            HashSet::new(),
                            SenderType::Legacy,
                        )
                        .await;
                }

                // Remove sender accounts
                for sender in state.sender_ids_v1.difference(&target_senders) {
                    if let Some(sender_handle) = ActorRef::<SenderAccountMessage>::where_is(
                        state.format_sender_account(sender, SenderType::Legacy),
                    ) {
                        sender_handle.stop(None);
                    }
                }

                state.sender_ids_v1 = target_senders;
            }

            SenderAccountsManagerMessage::UpdateSenderAccountsV2(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids_v2) {
                    state
                        .create_or_deny_sender(
                            myself.get_cell(),
                            *sender,
                            HashSet::new(),
                            SenderType::Horizon,
                        )
                        .await;
                }

                // Remove sender accounts
                for sender in state.sender_ids_v2.difference(&target_senders) {
                    if let Some(sender_handle) = ActorRef::<SenderAccountMessage>::where_is(
                        state.format_sender_account(sender, SenderType::Horizon),
                    ) {
                        sender_handle.stop(None);
                    }
                }

                state.sender_ids_v2 = target_senders;
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
    ) -> Result<(), ActorProcessingErr> {
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
                let mut splitter = sender_id.split(':');
                let Some(sender_id) = splitter.next_back() else {
                    tracing::error!(%sender_id, "Could not extract sender_id from name");
                    return Ok(());
                };
                let Ok(sender_id) = Address::parse_checksummed(sender_id, None) else {
                    tracing::error!(%sender_id, "Could not convert sender_id to Address");
                    return Ok(());
                };
                let sender_type = match splitter.next_back() {
                    Some("legacy") => SenderType::Legacy,
                    Some("horizon") => SenderType::Horizon,
                    _ => {
                        tracing::error!(%sender_id, "Could not extract sender_type from name");
                        return Ok(());
                    }
                };

                // Get the sender's allocations taking into account
                // the sender type
                let allocations = match sender_type {
                    SenderType::Legacy => {
                        let mut sender_allocation = select! {
                            sender_allocation = state.get_pending_sender_allocation_id_v1() => sender_allocation,
                            _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                                tracing::error!(version = "V1", "Timeout while getting pending sender allocation ids");
                                return Ok(());
                            }
                        };
                        sender_allocation
                            .remove(&sender_id)
                            .unwrap_or(HashSet::new())
                    }
                    SenderType::Horizon => {
                        if !state.config.horizon_enabled {
                            tracing::info!(%sender_id, "Horizon sender failed but horizon is disabled, not restarting");

                            return Ok(());
                        }

                        let mut sender_allocation = select! {
                            sender_allocation = state.get_pending_sender_allocation_id_v2() => sender_allocation,
                            _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                                tracing::error!(version = "V2", "Timeout while getting pending sender allocation ids");
                                return Ok(());
                            }
                        };
                        sender_allocation
                            .remove(&sender_id)
                            .unwrap_or(HashSet::new())
                    }
                };

                state
                    .create_or_deny_sender(myself.get_cell(), sender_id, allocations, sender_type)
                    .await;
            }
            _ => {}
        }
        Ok(())
    }
}

impl State {
    fn format_sender_account(&self, sender: &Address, sender_type: SenderType) -> String {
        let mut sender_allocation_id = String::new();
        if let Some(prefix) = &self.prefix {
            sender_allocation_id.push_str(prefix);
            sender_allocation_id.push(':');
        }
        sender_allocation_id.push_str(match sender_type {
            SenderType::Legacy => "legacy:",
            SenderType::Horizon => "horizon:",
        });
        sender_allocation_id.push_str(&format!("{}", sender));
        sender_allocation_id
    }

    /// Helper function to create a [SenderAccount]
    ///
    /// It takes the current [SenderAccountsManager] cell to use it
    /// as supervisor, sender address and a list of initial allocations
    ///
    /// In case there's an error creating it, deny so it
    /// can no longer send queries
    async fn create_or_deny_sender(
        &self,
        supervisor: ActorCell,
        sender_id: Address,
        allocation_ids: HashSet<AllocationId>,
        sender_type: SenderType,
    ) {
        if let Err(e) = self
            .create_sender_account(supervisor, sender_id, allocation_ids, sender_type)
            .await
        {
            tracing::error!(
                "There was an error while starting the sender {}, denying it. Error: {:?}",
                sender_id,
                e
            );
            SenderAccount::deny_sender(sender_type, &self.pgpool, sender_id).await;
        }
    }

    /// Helper function to create a [SenderAccount]
    ///
    /// It takes the current [SenderAccountsManager] cell to use it
    /// as supervisor, sender address and a list of initial allocations
    ///
    async fn create_sender_account(
        &self,
        supervisor: ActorCell,
        sender_id: Address,
        allocation_ids: HashSet<AllocationId>,
        sender_type: SenderType,
    ) -> anyhow::Result<()> {
        let Ok(args) = self.new_sender_account_args(&sender_id, allocation_ids, sender_type) else {
            tracing::warn!(
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
            Some(self.format_sender_account(&sender_id, sender_type)),
            SenderAccount,
            args,
            supervisor,
        )
        .await?;
        Ok(())
    }

    /// Gather all outstanding receipts and unfinalized RAVs from the database.
    /// Used to create [SenderAccount] instances for all senders that have unfinalized allocations
    /// and try to finalize them if they have become ineligible.
    ///
    /// This loads legacy allocations
    async fn get_pending_sender_allocation_id_v1(&self) -> HashMap<Address, HashSet<AllocationId>> {
        // First we accumulate all allocations for each sender. This is because we may have more
        // than one signer per sender in DB.
        let mut unfinalized_sender_allocations_map: HashMap<Address, HashSet<AllocationId>> =
            HashMap::new();

        let receipts_signer_allocations_in_db = sqlx::query!(
            r#"
                WITH grouped AS (
                    SELECT signer_address, allocation_id
                    FROM scalar_tap_receipts
                    GROUP BY signer_address, allocation_id
                )
                SELECT 
                    signer_address,
                    ARRAY_AGG(allocation_id) AS allocation_ids
                FROM grouped
                GROUP BY signer_address
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch pending receipts V1 from the database");

        for row in receipts_signer_allocations_in_db {
            let allocation_ids = row
                .allocation_ids
                .expect("all receipts V1 should have an allocation_id")
                .iter()
                .map(|allocation_id| {
                    AllocationId::Legacy(
                        AllocationIdCore::from_str(allocation_id)
                            .expect("allocation_id should be a valid allocation ID"),
                    )
                })
                .collect::<HashSet<_>>();
            let signer_id = Address::from_str(&row.signer_address)
                .expect("signer_address should be a valid address");
            let sender_id = self
                .escrow_accounts_v1
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
                SELECT
                    sender_address,
                    ARRAY_AGG(DISTINCT allocation_id) FILTER (WHERE NOT last) AS allocation_ids
                FROM scalar_tap_ravs
                GROUP BY sender_address
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch unfinalized RAVs V1 from the database");

        for row in nonfinal_ravs_sender_allocations_in_db {
            // Check if allocation_ids is Some before processing,
            // as ARRAY_AGG with FILTER returns NULL
            // instead of an empty array
            if let Some(allocation_id_strings) = row.allocation_ids {
                let allocation_ids = allocation_id_strings
                    .iter()
                    .map(|allocation_id| {
                        AllocationId::Legacy(
                            AllocationIdCore::from_str(allocation_id)
                                .expect("allocation_id should be a valid allocation ID"),
                        )
                    })
                    .collect::<HashSet<_>>();

                if !allocation_ids.is_empty() {
                    let sender_id = Address::from_str(&row.sender_address)
                        .expect("sender_address should be a valid address");

                    unfinalized_sender_allocations_map
                        .entry(sender_id)
                        .or_default()
                        .extend(allocation_ids);
                }
            } else {
                // Log the case when allocation_ids is NULL
                tracing::warn!(
                    "Found NULL allocation_ids. This may indicate all RAVs are finalized."
                );
            }
        }
        unfinalized_sender_allocations_map
    }

    /// Gather all outstanding receipts and unfinalized RAVs from the database.
    /// Used to create [SenderAccount] instances for all senders that have unfinalized allocations
    /// and try to finalize them if they have become ineligible.
    ///
    /// This loads horizon allocations
    async fn get_pending_sender_allocation_id_v2(&self) -> HashMap<Address, HashSet<AllocationId>> {
        // First we accumulate all allocations for each sender. This is because we may have more
        // than one signer per sender in DB.
        let mut unfinalized_sender_allocations_map: HashMap<Address, HashSet<AllocationId>> =
            HashMap::new();

        let receipts_signer_collections_in_db = sqlx::query!(
            r#"
                WITH grouped AS (
                    SELECT signer_address, collection_id
                    FROM tap_horizon_receipts
                    GROUP BY signer_address, collection_id
                )
                SELECT 
                    signer_address,
                    ARRAY_AGG(collection_id) AS collection_ids
                FROM grouped
                GROUP BY signer_address
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch pending V2 receipts from the database");

        for row in receipts_signer_collections_in_db {
            let collection_ids = row
                .collection_ids
                .expect("all receipts V2 should have a collection_id")
                .iter()
                .map(|collection_id| {
                    AllocationId::Horizon(
                        CollectionId::from_str(collection_id)
                            .expect("collection_id should be a valid collection ID"),
                    )
                })
                .collect::<HashSet<_>>();
            let signer_id = Address::from_str(&row.signer_address)
                .expect("signer_address should be a valid address");
            let sender_id = self
                .escrow_accounts_v2
                .borrow()
                .get_sender_for_signer(&signer_id)
                .expect("should be able to get sender from signer");

            // Accumulate allocations for the sender
            unfinalized_sender_allocations_map
                .entry(sender_id)
                .or_default()
                .extend(collection_ids);
        }

        let nonfinal_ravs_sender_allocations_in_db = sqlx::query!(
            r#"
                SELECT
                    payer,
                    ARRAY_AGG(DISTINCT collection_id) FILTER (WHERE NOT last) AS allocation_ids
                FROM tap_horizon_ravs
                GROUP BY payer
            "#
        )
        .fetch_all(&self.pgpool)
        .await
        .expect("should be able to fetch unfinalized V2 RAVs from the database");

        for row in nonfinal_ravs_sender_allocations_in_db {
            // Check if allocation_ids is Some before processing,
            // as ARRAY_AGG with FILTER returns NULL instead of an
            // empty array
            if let Some(allocation_id_strings) = row.allocation_ids {
                let allocation_ids = allocation_id_strings
                    .iter()
                    .map(|collection_id| {
                        AllocationId::Horizon(
                            CollectionId::from_str(collection_id)
                                .expect("collection_id should be a valid collection ID"),
                        )
                    })
                    .collect::<HashSet<_>>();

                if !allocation_ids.is_empty() {
                    let sender_id = Address::from_str(&row.payer)
                        .expect("sender_address should be a valid address");

                    unfinalized_sender_allocations_map
                        .entry(sender_id)
                        .or_default()
                        .extend(allocation_ids);
                }
            } else {
                // Log the case when allocation_ids is NULL
                tracing::warn!(
                    "Found NULL allocation_ids. This may indicate all RAVs are finalized."
                );
            }
        }
        unfinalized_sender_allocations_map
    }

    /// Helper function to create [SenderAccountArgs]
    ///
    /// Fails if the provided sender_id is not present
    /// in the sender_aggregator_endpoints map
    fn new_sender_account_args(
        &self,
        sender_id: &Address,
        allocation_ids: HashSet<AllocationId>,
        sender_type: SenderType,
    ) -> anyhow::Result<SenderAccountArgs> {
        Ok(SenderAccountArgs {
            config: self.config,
            pgpool: self.pgpool.clone(),
            sender_id: *sender_id,
            escrow_accounts: match sender_type {
                SenderType::Legacy => self.escrow_accounts_v1.clone(),
                SenderType::Horizon => self.escrow_accounts_v2.clone(),
            },
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
            sender_type,
        })
    }
}

/// Continuously listens for new receipt notifications from Postgres and forwards them to the
/// corresponding SenderAccount.
#[bon::builder]
async fn new_receipts_watcher(
    actor_cell: ActorCell,
    mut pglistener: PgListener,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    sender_type: SenderType,
    prefix: Option<String>,
) {
    match sender_type {
        SenderType::Legacy => {
            pglistener
                .listen("scalar_tap_receipt_notification")
                .await
                .expect(
                    "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
                );
        }
        SenderType::Horizon => {
            pglistener
                .listen("tap_horizon_receipt_notification")
                .await
                .expect(
                    "should be able to subscribe to Postgres Notify events on the channel \
                'tap_horizon_receipt_notification'",
                );
        }
    }
    loop {
        let Ok(pg_notification) = pglistener.recv().await else {
            tracing::error!(
                "should be able to receive Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'/'tap_horizon_receipt_notification'"
            );
            break;
        };
        let Ok(new_receipt_notification) =
            serde_json::from_str::<NewReceiptNotification>(pg_notification.payload())
        else {
            tracing::error!(
                "should be able to deserialize the Postgres Notify event payload as a \
                        NewReceiptNotification",
            );
            break;
        };
        if let Err(e) = handle_notification(
            new_receipt_notification,
            escrow_accounts_rx.clone(),
            sender_type,
            prefix.as_deref(),
        )
        .await
        {
            tracing::error!("{}", e);
        }
    }
    // shutdown the whole system
    actor_cell
        .kill_and_wait(None)
        .await
        .expect("Failed to kill manager.");
    tracing::error!("Manager killed");
}

/// Handles a new detected [NewReceiptNotification] and routes to proper
/// reference of [super::sender_allocation::SenderAllocation]
///
/// If the allocation doesn't exist yet, we trust that the whoever has
/// access to the database already verified that the allocation really
/// exists and we ask for the sender to create a new allocation.
///
/// After a request to create allocation, we don't need to do anything
/// since the startup script is going to recalculate the receipt in the
/// database
async fn handle_notification(
    new_receipt_notification: NewReceiptNotification,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    sender_type: SenderType,
    prefix: Option<&str>,
) -> anyhow::Result<()> {
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
        tracing::warn!(
            "No sender_allocation found for sender_address {}, allocation_id {} to process new \
                receipt notification. Starting a new sender_allocation.",
            sender_address,
            allocation_id
        );
        let sender_account_name = format!(
            "{}{}{sender_address}",
            prefix
                .as_ref()
                .map_or(String::default(), |prefix| format!("{prefix}:")),
            match sender_type {
                SenderType::Legacy => "legacy:",
                SenderType::Horizon => "horizon:",
            }
        );

        let Some(sender_account) = ActorRef::<SenderAccountMessage>::where_is(sender_account_name)
        else {
            bail!(
                "No sender_account was found for address: {}.",
                sender_address
            );
        };
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(match sender_type {
                SenderType::Legacy => AllocationId::Legacy(AllocationIdCore::from(*allocation_id)),
                SenderType::Horizon => {
                    // For now, convert Address to CollectionId for Horizon
                    // This is a temporary fix - in production the notification should contain CollectionId
                    let collection_id_str = format!(
                        "000000000000000000000000{}",
                        allocation_id.encode_hex_with_prefix()
                    );
                    AllocationId::Horizon(
                        CollectionId::from_str(&collection_id_str[2..])
                            .expect("Failed to convert address to collection ID"),
                    )
                }
            }))
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use ractor::{Actor, ActorRef, ActorStatus};
    use reqwest::Url;
    use ruint::aliases::U256;
    use serial_test::serial;
    use sqlx::{postgres::PgListener, PgPool};
    use test_assets::{
        assert_while_retry, flush_messages, pgpool, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER,
    };
    use thegraph_core::alloy::hex::ToHexExt;
    use tokio::sync::{
        mpsc::{self, error::TryRecvError},
        watch,
    };

    use super::{new_receipts_watcher, SenderAccountsManagerMessage, State};
    use crate::{
        agent::{
            sender_account::SenderAccountMessage,
            sender_accounts_manager::{handle_notification, NewReceiptNotification, SenderType},
        },
        test::{
            actors::{DummyActor, MockSenderAccount, MockSenderAllocation, TestableActor},
            create_rav, create_received_receipt, create_sender_accounts_manager,
            generate_random_prefix, get_grpc_url, get_sender_account_config, store_rav,
            store_receipt, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER, SENDER_2,
            TAP_EIP712_DOMAIN_SEPARATOR,
        },
    };
    const DUMMY_URL: &str = "http://localhost:1234";

    async fn get_subgraph_client() -> &'static SubgraphClient {
        Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(DUMMY_URL).unwrap(),
            )
            .await,
        ))
    }

    struct TestState {
        prefix: String,
        state: State,
    }

    #[rstest::fixture]
    async fn state(#[future(awt)] pgpool: PgPool) -> TestState {
        let (prefix, state) = create_state(pgpool.clone()).await;
        TestState { prefix, state }
    }
    #[rstest::fixture]
    async fn receipts(#[future(awt)] pgpool: PgPool) {
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool.clone(), receipt.signed_receipt())
                .await
                .unwrap();
        }
    }
    #[rstest::fixture]
    async fn supervisor() -> ActorRef<()> {
        DummyActor::spawn().await
    }

    #[rstest::fixture]
    pub async fn pglistener(#[future(awt)] pgpool: PgPool) -> PgListener {
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );
        pglistener
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_create_sender_accounts_manager(pgpool: PgPool) {
        let (_, _, (actor, join_handle)) =
            create_sender_accounts_manager().pgpool(pgpool).call().await;
        actor.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }

    async fn create_state(pgpool: PgPool) -> (String, State) {
        let config = get_sender_account_config();
        let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
        let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);

        let prefix = generate_random_prefix();
        (
            prefix.clone(),
            State {
                config,
                domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
                sender_ids_v1: HashSet::new(),
                sender_ids_v2: HashSet::new(),
                new_receipts_watcher_handle_v1: None,
                new_receipts_watcher_handle_v2: None,
                pgpool,
                indexer_allocations: watch::channel(HashSet::new()).1,
                escrow_accounts_v1: watch::channel(escrow_accounts.clone()).1,
                escrow_accounts_v2: watch::channel(escrow_accounts).1,
                escrow_subgraph: get_subgraph_client().await,
                network_subgraph: get_subgraph_client().await,
                sender_aggregator_endpoints: HashMap::from([
                    (SENDER.1, Url::parse(&get_grpc_url().await).unwrap()),
                    (SENDER_2.1, Url::parse(&get_grpc_url().await).unwrap()),
                ]),
                prefix: Some(prefix),
            },
        )
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_pending_sender_allocations(#[future(awt)] pgpool: PgPool) {
        let (_, state) = create_state(pgpool.clone()).await;
        // add receipts to the database
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        // add non-final ravs
        let signed_rav = create_rav(ALLOCATION_ID_1, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        let pending_allocation_id = state.get_pending_sender_allocation_id_v1().await;

        // check if pending allocations are correct
        assert_eq!(pending_allocation_id.len(), 1);
        assert!(pending_allocation_id.contains_key(&SENDER.1));
        assert_eq!(pending_allocation_id.get(&SENDER.1).unwrap().len(), 2);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[serial]
    async fn test_update_sender_account(pgpool: PgPool) {
        let (prefix, mut notify, (actor, join_handle)) =
            create_sender_accounts_manager().pgpool(pgpool).call().await;

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV1(
                vec![SENDER.1].into_iter().collect(),
            ))
            .unwrap();

        flush_messages(&mut notify).await;

        assert_while_retry! {
            ActorRef::<SenderAccountMessage>::where_is(format!(
                "{}:legacy:{}",
                prefix.clone(),
                SENDER.1
            )).is_none()
        };

        // verify if create sender account
        let sender_ref = ActorRef::<SenderAccountMessage>::where_is(format!(
            "{}:legacy:{}",
            prefix.clone(),
            SENDER.1
        ))
        .unwrap();

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV1(
                HashSet::new(),
            ))
            .unwrap();

        flush_messages(&mut notify).await;

        sender_ref.wait(None).await.unwrap();
        // verify if it gets removed
        let actor_ref =
            ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER.1));
        assert!(actor_ref.is_none());

        // safely stop the manager
        actor.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_create_sender_account(
        #[future(awt)] state: TestState,
        #[future(awt)] supervisor: ActorRef<()>,
    ) {
        // we wait to check if the sender is created
        state
            .state
            .create_sender_account(
                supervisor.get_cell(),
                SENDER_2.1,
                HashSet::new(),
                SenderType::Legacy,
            )
            .await
            .unwrap();

        let actor_ref = ActorRef::<SenderAccountMessage>::where_is(format!(
            "{}:legacy:{}",
            state.prefix, SENDER_2.1
        ));
        assert!(actor_ref.is_some());
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_deny_sender_account_on_failure(
        #[future(awt)] pgpool: PgPool,
        #[future(awt)] supervisor: ActorRef<()>,
    ) {
        let (_prefix, state) = create_state(pgpool.clone()).await;
        state
            .create_or_deny_sender(
                supervisor.get_cell(),
                INDEXER.1,
                HashSet::new(),
                SenderType::Legacy,
            )
            .await;

        let denied = sqlx::query!(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM scalar_tap_denylist
                    WHERE sender_address = $1
                ) as denied
            "#,
            INDEXER.1.encode_hex(),
        )
        .fetch_one(&pgpool)
        .await
        .unwrap()
        .denied
        .expect("Deny status cannot be null");

        assert!(denied, "Sender was not denied after failing.");
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_receive_notifications(#[future(awt)] pgpool: PgPool) {
        let prefix = generate_random_prefix();
        // create dummy allocation

        let (mock_sender_allocation, mut receipts) = MockSenderAllocation::new_with_receipts();
        let (tx, mut notify) = mpsc::channel(10);
        let actor = TestableActor::new(mock_sender_allocation, tx);
        let _ = Actor::spawn(
            Some(format!(
                "{}:{}:{}",
                prefix.clone(),
                SENDER.1,
                ALLOCATION_ID_0
            )),
            actor,
            (),
        )
        .await
        .unwrap();

        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
            'scalar_tap_receipt_notification'",
            );

        let escrow_accounts_rx = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;
        let dummy_actor = DummyActor::spawn().await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(
            new_receipts_watcher()
                .actor_cell(dummy_actor.get_cell())
                .pglistener(pglistener)
                .escrow_accounts_rx(escrow_accounts_rx)
                .sender_type(SenderType::Legacy)
                .prefix(prefix.clone())
                .call(),
        );

        let receipts_count = 10;
        // add receipts to the database
        for i in 1..=receipts_count {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        flush_messages(&mut notify).await;

        // check if receipt notification was sent to the allocation
        for i in 1..=receipts_count {
            let receipt = receipts.recv().await.unwrap();

            assert_eq!(i, receipt.id);
        }
        assert_eq!(receipts.try_recv().unwrap_err(), TryRecvError::Empty);

        new_receipts_watcher_handle.abort();
    }

    #[rstest::rstest]
    #[tokio::test]
    #[serial]
    async fn test_manager_killed_in_database_connection(#[future(awt)] pgpool: PgPool) {
        let mut pglistener = PgListener::connect_with(&pgpool).await.unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );

        let escrow_accounts_rx = watch::channel(EscrowAccounts::default()).1;
        let dummy_actor = DummyActor::spawn().await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(
            new_receipts_watcher()
                .sender_type(SenderType::Legacy)
                .actor_cell(dummy_actor.get_cell())
                .pglistener(pglistener)
                .escrow_accounts_rx(escrow_accounts_rx)
                .call(),
        );
        pgpool.close().await;
        new_receipts_watcher_handle.await.unwrap();

        assert_eq!(dummy_actor.get_status(), ActorStatus::Stopped)
    }

    #[tokio::test]
    #[serial]
    async fn test_create_allocation_id() {
        let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
        let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);
        let escrow_accounts = watch::channel(escrow_accounts).1;

        let prefix = generate_random_prefix();

        let (last_message_emitted, mut rx) = mpsc::channel(64);

        let (sender_account, join_handle) = MockSenderAccount::spawn(
            Some(format!("{}:legacy:{}", prefix.clone(), SENDER.1,)),
            MockSenderAccount {
                last_message_emitted,
            },
            (),
        )
        .await
        .unwrap();

        let new_receipt_notification = NewReceiptNotification {
            id: 1,
            allocation_id: ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            timestamp_ns: 1,
            value: 1,
        };

        handle_notification(
            new_receipt_notification,
            escrow_accounts,
            SenderType::Legacy,
            Some(&prefix),
        )
        .await
        .unwrap();

        let new_alloc_msg = rx.recv().await.unwrap();
        insta::assert_debug_snapshot!(new_alloc_msg);
        sender_account.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }
}
