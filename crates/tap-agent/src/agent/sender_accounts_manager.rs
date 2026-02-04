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

pub(crate) static RECEIPTS_CREATED: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "tap_receipts_received_total",
        "Receipts received since start of the program.",
        &["sender", "allocation"]
    )
    .unwrap()
});

const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Notification received by pgnotify for V2 (Horizon) receipts
///
/// This contains a list of properties that are sent by postgres when a V2 receipt is inserted
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct NewReceiptNotification {
    /// id inside the table
    pub id: u64,
    /// collection id (V2 uses 32-byte collection_id)
    pub collection_id: String, // 64-character hex string from database
    /// address of wallet that signed this receipt
    pub signer_address: Address,
    /// timestamp of the receipt
    pub timestamp_ns: u64,
    /// value of the receipt
    pub value: u128,
}

impl NewReceiptNotification {
    /// Get the ID regardless of version
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get the signer address regardless of version
    pub fn signer_address(&self) -> Address {
        self.signer_address
    }

    /// Get the timestamp regardless of version
    pub fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    /// Get the value regardless of version
    pub fn value(&self) -> u128 {
        self.value
    }

    /// Get the allocation ID for V2 receipts.
    #[tracing::instrument(skip(self), ret)]
    pub fn allocation_id(&self) -> anyhow::Result<AllocationId> {
        // Convert the hex string to CollectionId (trim spaces from fixed-length DB field)
        let trimmed = self.collection_id.trim();
        let hex_str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        if hex_str.len() != 64 {
            bail!(
                "Invalid collection_id length: expected 64 hex chars, got {} ({})",
                hex_str.len(),
                trimmed
            );
        }

        let normalized = format!("0x{hex_str}");
        let collection_id = CollectionId::from_str(&normalized)
            .map_err(|e| anyhow!("Failed to parse collection_id '{trimmed}': {e}"))?;
        Ok(AllocationId::Horizon(collection_id))
    }
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
    /// Canonical hex (no 0x); 40 chars for Legacy, 64 for Horizon
    pub fn to_hex(&self) -> String {
        match self {
            AllocationId::Legacy(allocation_id) => (**allocation_id).encode_hex(),
            AllocationId::Horizon(collection_id) => collection_id.encode_hex(),
        }
    }

    /// Get the underlying Address for Legacy allocations.
    ///
    /// Deprecated: Prefer `address()` which returns a normalized Address for both Legacy and Horizon.
    #[deprecated(
        note = "Use `address()` for both Legacy and Horizon; this returns None for Horizon"
    )]
    pub fn as_address(&self) -> Option<Address> {
        match self {
            AllocationId::Legacy(allocation_id) => Some(**allocation_id),
            AllocationId::Horizon(_) => None,
        }
    }

    /// Legacy-only accessor returning an optional address.
    ///
    /// Returns:
    /// - Some(address) for Legacy allocations
    /// - None for Horizon allocations
    pub fn legacy_address(&self) -> Option<Address> {
        match self {
            AllocationId::Legacy(allocation_id) => Some(**allocation_id),
            AllocationId::Horizon(_) => None,
        }
    }

    /// Get an Address representation for both allocation types
    pub fn address(&self) -> Address {
        match self {
            AllocationId::Legacy(allocation_id) => **allocation_id,
            AllocationId::Horizon(collection_id) => {
                AllocationIdCore::from(*collection_id).into_inner()
            }
        }
    }

    /// Normalized 20-byte address as lowercase hex (no 0x prefix).
    ///
    /// Behavior:
    /// - Legacy (V1): returns the allocation address as hex.
    /// - Horizon (V2): derives the 20-byte address from the 32-byte `CollectionId`
    ///   via `thegraph_core::AllocationId::from(collection_id)` (last 20 bytes) and encodes as hex.
    ///
    /// Use for:
    /// - Actor names and routing (consistent identity across versions)
    /// - Metrics labels (uniform 20-byte form)
    /// - Network subgraph queries (which expect allocation addresses)
    ///
    /// Do NOT use for Horizon database queries where `collection_id` is stored
    /// as 32-byte hex; use `to_hex()` / `CollectionId::encode_hex()` instead.
    pub fn address_hex(&self) -> String {
        self.address().encode_hex()
    }
}

impl Display for AllocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocationId::Legacy(allocation_id) => write!(f, "{allocation_id}"),
            AllocationId::Horizon(collection_id) => write!(f, "{collection_id}"),
        }
    }
}

/// Enum containing all types of messages that a [SenderAccountsManager] can receive
#[derive(Debug)]
#[cfg_attr(any(test, feature = "test"), derive(Clone))]
pub enum SenderAccountsManagerMessage {
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
    /// Domain separator used for tap v2 (Horizon)
    pub domain_separator_v2: Eip712Domain,

    /// Database connection
    pub pgpool: PgPool,
    /// Watcher that returns a map of open and recently closed allocation ids
    pub indexer_allocations: Receiver<HashMap<Address, Allocation>>,
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
    sender_ids_v2: HashSet<Address>,
    new_receipts_watcher_handle_v2: Option<tokio::task::JoinHandle<()>>,

    config: &'static SenderAccountConfig,
    domain_separator_v2: Eip712Domain,
    pgpool: PgPool,
    // Raw allocation watcher (address -> Allocation). Normalized per-sender later.
    indexer_allocations: Receiver<HashMap<Address, Allocation>>,
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
            domain_separator_v2,
            indexer_allocations,
            pgpool,
            escrow_accounts_v2,
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix,
        }: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Do not pre-map allocations globally. We keep the raw watcher and
        // normalize per SenderAccount.
        tracing::info!(
            horizon_active = %config.tap_mode.is_horizon(),
            "Using raw indexer_allocations watcher; normalization happens per sender"
        );
        // We only listen to Horizon notifications.
        let pglistener_v2 = PgListener::connect_with(&pgpool.clone()).await.unwrap();

        let myself_clone = myself.clone();
        let _escrow_accounts_v2 = escrow_accounts_v2.clone();
        watch_pipe(_escrow_accounts_v2, move |escrow_accounts| {
            let senders = escrow_accounts.get_senders();
            myself_clone
                .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV2(
                    senders,
                ))
                .unwrap_or_else(|e| {
                    tracing::error!(error = ?e, "Error while updating sender_accounts v2");
                });
            async {}
        });

        let mut state = State {
            config,
            domain_separator_v2,
            sender_ids_v2: HashSet::new(),
            new_receipts_watcher_handle_v2: None,
            pgpool: pgpool.clone(),
            indexer_allocations,
            escrow_accounts_v2: escrow_accounts_v2.clone(),
            escrow_subgraph,
            network_subgraph,
            sender_aggregator_endpoints,
            prefix: prefix.clone(),
        };
        let sender_allocation_v2 = select! {
            sender_allocation = state.get_pending_sender_allocation_id_v2() => sender_allocation,
            _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                panic!("Timeout while getting pending sender allocation ids");
            }
        };

        state.sender_ids_v2.extend(sender_allocation_v2.keys());
        stream::iter(sender_allocation_v2)
            .map(|(sender_id, allocation_ids)| {
                state.create_or_deny_sender(myself.get_cell(), sender_id, allocation_ids)
            })
            .buffer_unordered(10) // Limit concurrency to 10 senders at a time
            .collect::<Vec<()>>()
            .await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        // after starting all senders
        state.new_receipts_watcher_handle_v2 = Some(tokio::spawn(
            new_receipts_watcher()
                .actor_cell(myself.get_cell())
                .pglistener(pglistener_v2)
                .escrow_accounts_rx(escrow_accounts_v2)
                .maybe_prefix(prefix)
                .call(),
        ));

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
            SenderAccountsManagerMessage::UpdateSenderAccountsV2(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids_v2) {
                    state
                        .create_or_deny_sender(myself.get_cell(), *sender, HashSet::new())
                        .await;
                }

                // Remove sender accounts
                for sender in state.sender_ids_v2.difference(&target_senders) {
                    if let Some(sender_handle) = ActorRef::<SenderAccountMessage>::where_is(
                        state.format_sender_account(sender),
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
                match splitter.next_back() {
                    Some("horizon") => {}
                    _ => {
                        tracing::error!(%sender_id, "Could not extract sender_type from name");
                        return Ok(());
                    }
                };

                let mut sender_allocation = select! {
                    sender_allocation = state.get_pending_sender_allocation_id_v2() => sender_allocation,
                    _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
                        tracing::error!(version = "V2", "Timeout while getting pending sender allocation ids");
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
        sender_allocation_id.push_str("horizon:");
        sender_allocation_id.push_str(&format!("{sender}"));
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
    ) {
        tracing::info!(
            sender = %sender_id,
            initial_allocations = allocation_ids.len(),
            "Creating SenderAccount",
        );
        for alloc_id in &allocation_ids {
            tracing::debug!(
                allocation_id = %alloc_id,
                variant = %match alloc_id { AllocationId::Legacy(_) => "Legacy", AllocationId::Horizon(_) => "Horizon" },
                address = %alloc_id.address(),
                "Initial allocation",
            );
        }

        if let Err(e) = self
            .create_sender_account(supervisor, sender_id, allocation_ids)
            .await
        {
            tracing::error!(
                "There was an error while starting the sender {}, denying it. Error: {:?}",
                sender_id,
                e
            );
            SenderAccount::deny_sender(&self.pgpool, sender_id).await;
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
    ) -> anyhow::Result<()> {
        let Ok(args) = self.new_sender_account_args(&sender_id, allocation_ids) else {
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
            Some(self.format_sender_account(&sender_id)),
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
                    WHERE data_service = $1 AND service_provider = $2
                    GROUP BY signer_address, collection_id
                )
                SELECT 
                    signer_address,
                    ARRAY_AGG(collection_id) AS collection_ids
                FROM grouped
                GROUP BY signer_address
            "#,
            self.config
                .tap_mode
                .require_subgraph_service_address()
                .encode_hex(),
            self.config.indexer_address.encode_hex()
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
                    let trimmed = collection_id.trim();
                    let hex_str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
                    if hex_str.len() != 64 {
                        panic!(
                            "Invalid collection_id length '{}': expected 64 hex characters, got {}",
                            trimmed,
                            hex_str.len()
                        )
                    }
                    let normalized = format!("0x{hex_str}");
                    AllocationId::Horizon(
                        CollectionId::from_str(&normalized)
                            .unwrap_or_else(|e| panic!("Invalid collection_id '{trimmed}': {e}")),
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
                WHERE data_service = $1 AND service_provider = $2
                GROUP BY payer
            "#,
            // Constrain to our Horizon bucket to avoid conflating RAVs across services/providers
            self.config
                .tap_mode
                .require_subgraph_service_address()
                .encode_hex(),
            self.config.indexer_address.encode_hex()
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
    ) -> anyhow::Result<SenderAccountArgs> {
        let escrow_accounts = self.escrow_accounts_v2.clone();

        // Build a normalized allocation watcher for Horizon using isLegacy flag
        // from the Network Subgraph.
        let indexer_allocations = {
            map_watcher(self.indexer_allocations.clone(), move |alloc_map| {
                let total = alloc_map.len();
                let mut legacy_count = 0usize;
                let mut horizon_count = 0usize;
                let mut mismatched = 0usize;
                let set: HashSet<AllocationId> = alloc_map
                    .iter()
                    .filter_map(|(addr, alloc)| {
                        if alloc.is_legacy {
                            legacy_count += 1;
                            mismatched += 1;
                            None
                        } else {
                            horizon_count += 1;
                            Some(AllocationId::Horizon(CollectionId::from(*addr)))
                        }
                    })
                    .collect();

                tracing::info!(
                    total,
                    legacy = legacy_count,
                    horizon = horizon_count,
                    mismatched,
                    normalized = set.len(),
                    "Normalized indexer allocations using isLegacy"
                );
                set
            })
        };

        Ok(SenderAccountArgs {
            config: self.config,
            pgpool: self.pgpool.clone(),
            sender_id: *sender_id,
            escrow_accounts,
            indexer_allocations,
            escrow_subgraph: self.escrow_subgraph,
            network_subgraph: self.network_subgraph,
            domain_separator_v2: self.domain_separator_v2.clone(),
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
            retry_interval: RETRY_INTERVAL,
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
    prefix: Option<String>,
) {
    pglistener
        .listen("tap_horizon_receipt_notification")
        .await
        .expect(
            "should be able to subscribe to Postgres Notify events on the channel \
            'tap_horizon_receipt_notification'",
        );

    tracing::info!(
        "New receipts watcher started and listening for Horizon notifications, prefix: {:?}",
        prefix
    );

    loop {
        tracing::debug!("Waiting for notification from pglistener...");

        let Ok(pg_notification) = pglistener.recv().await else {
            tracing::error!(
                "should be able to receive Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'/'tap_horizon_receipt_notification'"
            );
            break;
        };

        tracing::info!(
            channel = pg_notification.channel(),
            payload = pg_notification.payload(),
            "Received notification from database"
        );
        let new_receipt_notification =
            match serde_json::from_str::<NewReceiptNotification>(pg_notification.payload()) {
                Ok(v2_notif) => v2_notif,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        payload = pg_notification.payload(),
                        "Failed to deserialize V2 notification payload",
                    );
                    break;
                }
            };
        match handle_notification(
            new_receipt_notification,
            escrow_accounts_rx.clone(),
            prefix.as_deref(),
        )
        .await
        {
            Ok(()) => {
                tracing::debug!(
                    event = "notification_handled",
                    "Successfully handled notification"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "Error handling notification");
            }
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
#[tracing::instrument(
    skip_all,
    fields(
        sender_address = %new_receipt_notification.signer_address(),
    )
)]
async fn handle_notification(
    new_receipt_notification: NewReceiptNotification,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<&str>,
) -> anyhow::Result<()> {
    tracing::trace!(
        notification = ?new_receipt_notification,
        "New receipt notification detected!"
    );
    let escrow_accounts = escrow_accounts_rx.borrow();
    let signer = new_receipt_notification.signer_address();
    tracing::debug!(
        sender_type_str = "V2",
        signer = ?signer,
        "Looking up sender for signer in escrow accounts",
    );

    let Ok(sender_address) = escrow_accounts.get_sender_for_signer(&signer) else {
        tracing::error!(
            signer=?signer,
            sender_type_str = "V2",
            "ESCROW LOOKUP FAILURE: No sender found for signer in escrow accounts",
        );

        // TODO: save the receipt in the failed receipts table?
        bail!(
            "No sender address found for receipt signer address {} in {} escrow accounts. \
                    This suggests either: (1) escrow accounts not yet loaded or (2) signer not authorized.",
            signer,
            "V2",
        );
    };

    let allocation_id = new_receipt_notification.allocation_id()?;
    let allocation_str = allocation_id.to_hex();
    tracing::info!(
        sender_address = %sender_address,
        collection_id = %allocation_id,
        sender_type = "V2",
        receipt_value = %new_receipt_notification.value(),
        "Processing receipt notification",
    );

    // For actor lookup, use the address format that matches how actors are created
    // "0x...."
    let allocation_for_actor_name = allocation_id.address().to_string();

    let actor_name = format!(
        "{}{sender_address}:{allocation_for_actor_name}",
        prefix
            .as_ref()
            .map_or(String::default(), |prefix| format!("{prefix}:"))
    );

    // this logs must match regarding allocation type with
    // logs in   sender_account.rs:1174
    // otherwise there is a mistmatch!!!!
    tracing::debug!(
        actor_name,
        allocation_id = %allocation_id,
        variant = %match allocation_id { AllocationId::Legacy(_) => "Legacy", AllocationId::Horizon(_) => "Horizon" },
        "Looking for SenderAllocation actor",
    );

    let Some(sender_allocation) = ActorRef::<SenderAllocationMessage>::where_is(actor_name) else {
        tracing::warn!(
            sender_address=%sender_address,
            allocation_id=%allocation_id,
            "No sender_allocation found for sender_address and allocation_id to process new \
                receipt notification. Starting a new sender_allocation.",
        );

        let sender_account_name = format!(
            "{}{}{sender_address}",
            prefix
                .as_ref()
                .map_or(String::default(), |prefix| format!("{prefix}:")),
            "horizon:",
        );
        tracing::debug!(
            sender_account_name,
            allocation_id = %allocation_id,
            "Looking for SenderAccount",
        );

        let Some(sender_account) = ActorRef::<SenderAccountMessage>::where_is(sender_account_name)
        else {
            bail!(
                "No sender_account was found for address: {}.",
                sender_address
            );
        };
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(allocation_id))
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
        .with_label_values(&[&sender_address.to_string(), &allocation_str])
        .inc();
    Ok(())
}

/// Force initialization of all LazyLock metrics in this module.
///
/// This ensures metrics are registered with Prometheus at startup,
/// even if no receipts have been processed yet.
pub fn init_metrics() {
    // Dereference each LazyLock to force initialization
    let _ = &*RECEIPTS_CREATED;
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use ractor::{Actor, ActorRef, ActorStatus};
    use reqwest::Url;
    use ruint::aliases::U256;
    use sqlx::{postgres::PgListener, PgPool};
    use test_assets::{
        assert_while_retry, flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER,
    };
    use thegraph_core::{alloy::hex::ToHexExt, CollectionId};
    use tokio::sync::{
        mpsc::{self, error::TryRecvError},
        watch,
    };

    use super::{
        new_receipts_watcher, NewReceiptNotification, SenderAccountsManagerMessage, State,
    };
    use crate::{
        agent::{
            sender_account::SenderAccountMessage, sender_accounts_manager::handle_notification,
        },
        test::{
            actors::{DummyActor, MockSenderAccount, MockSenderAllocation, TestableActor},
            create_rav_v2, create_received_receipt_v2, create_sender_accounts_manager,
            generate_random_prefix, get_grpc_url, get_sender_account_config, store_rav_v2,
            store_receipt, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER, SENDER_2,
            TAP_EIP712_DOMAIN_SEPARATOR_V2,
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
        _test_db: test_assets::TestDatabase,
    }

    async fn setup_state() -> TestState {
        let test_db = test_assets::setup_shared_test_db().await;
        let (prefix, state) = create_state(test_db.pool.clone()).await;
        TestState {
            prefix,
            state,
            _test_db: test_db,
        }
    }
    async fn setup_supervisor() -> ActorRef<()> {
        DummyActor::spawn().await
    }

    #[tokio::test]
    async fn test_create_sender_accounts_manager() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
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
                domain_separator_v2: TAP_EIP712_DOMAIN_SEPARATOR_V2.clone(),
                sender_ids_v2: HashSet::new(),
                new_receipts_watcher_handle_v2: None,
                pgpool,
                indexer_allocations: watch::channel(HashMap::new()).1,
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

    #[tokio::test]
    async fn test_pending_sender_allocations() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let (_, state) = create_state(pgpool.clone()).await;
        // add receipts to the database
        for i in 1..=10 {
            let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        // add non-final ravs
        let signed_rav = create_rav_v2(
            *CollectionId::from(ALLOCATION_ID_1),
            SIGNER.0.clone(),
            4,
            10,
        );
        store_rav_v2(&pgpool, signed_rav, SENDER.1).await.unwrap();

        let pending_allocation_id = state.get_pending_sender_allocation_id_v2().await;

        // check if pending allocations are correct
        assert_eq!(pending_allocation_id.len(), 1);
        assert!(pending_allocation_id.contains_key(&SENDER.1));
        assert_eq!(pending_allocation_id.get(&SENDER.1).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_update_sender_account() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let (prefix, mut notify, (actor, join_handle)) =
            create_sender_accounts_manager().pgpool(pgpool).call().await;

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV2(
                vec![SENDER.1].into_iter().collect(),
            ))
            .unwrap();

        flush_messages(&mut notify).await;

        assert_while_retry! {
            ActorRef::<SenderAccountMessage>::where_is(format!(
                "{}:horizon:{}",
                prefix.clone(),
                SENDER.1
            )).is_none()
        };

        // verify if create sender account
        let sender_ref = ActorRef::<SenderAccountMessage>::where_is(format!(
            "{}:horizon:{}",
            prefix.clone(),
            SENDER.1
        ))
        .unwrap();

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccountsV2(
                HashSet::new(),
            ))
            .unwrap();

        flush_messages(&mut notify).await;

        sender_ref.wait(None).await.unwrap();
        // verify if it gets removed
        let actor_ref =
            ActorRef::<SenderAccountMessage>::where_is(format!("{}:horizon:{}", prefix, SENDER.1));
        assert!(actor_ref.is_none());

        // safely stop the manager
        actor.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_create_sender_account() {
        let state = setup_state().await;
        let supervisor = setup_supervisor().await;
        // we wait to check if the sender is created
        state
            .state
            .create_sender_account(supervisor.get_cell(), SENDER_2.1, HashSet::new())
            .await
            .unwrap();

        let actor_ref = ActorRef::<SenderAccountMessage>::where_is(format!(
            "{}:horizon:{}",
            state.prefix, SENDER_2.1
        ));
        assert!(actor_ref.is_some());
    }

    #[tokio::test]
    async fn test_deny_sender_account_on_failure() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let supervisor = DummyActor::spawn().await;
        let (_prefix, state) = create_state(pgpool.clone()).await;
        state
            .create_or_deny_sender(supervisor.get_cell(), INDEXER.1, HashSet::new())
            .await;

        let denied = sqlx::query!(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM tap_horizon_denylist
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

    #[tokio::test]
    async fn test_receive_notifications() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
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
            .listen("tap_horizon_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
            'tap_horizon_receipt_notification'",
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
                .prefix(prefix.clone())
                .call(),
        );

        let receipts_count = 10;
        // add receipts to the database
        for i in 1..=receipts_count {
            let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        flush_messages(&mut notify).await;

        // check if receipt notification was sent to the allocation
        for i in 1..=receipts_count {
            let receipt = receipts.recv().await.unwrap();

            assert_eq!(i, receipt.id());
        }
        assert_eq!(receipts.try_recv().unwrap_err(), TryRecvError::Empty);

        new_receipts_watcher_handle.abort();
    }

    #[tokio::test]
    async fn test_manager_killed_in_database_connection() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let mut pglistener = PgListener::connect_with(&pgpool).await.unwrap();
        pglistener
            .listen("tap_horizon_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'tap_horizon_receipt_notification'",
            );

        let escrow_accounts_rx = watch::channel(EscrowAccounts::default()).1;
        let dummy_actor = DummyActor::spawn().await;

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(
            new_receipts_watcher()
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
    async fn test_create_allocation_id() {
        let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
        let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);
        let escrow_accounts = watch::channel(escrow_accounts).1;

        let prefix = generate_random_prefix();

        let (last_message_emitted, mut rx) = mpsc::channel(64);

        let (sender_account, join_handle) = MockSenderAccount::spawn(
            Some(format!("{}:horizon:{}", prefix.clone(), SENDER.1,)),
            MockSenderAccount {
                last_message_emitted,
            },
            (),
        )
        .await
        .unwrap();

        let collection_id = CollectionId::from(ALLOCATION_ID_0).encode_hex();
        let new_receipt_notification = NewReceiptNotification {
            id: 1,
            collection_id,
            signer_address: SIGNER.1,
            timestamp_ns: 1,
            value: 1,
        };

        handle_notification(new_receipt_notification, escrow_accounts, Some(&prefix))
            .await
            .unwrap();

        let new_alloc_msg = rx.recv().await.unwrap();
        insta::assert_debug_snapshot!(new_alloc_msg);
        sender_account.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }
}
