// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use anyhow::{anyhow, bail};
use futures::{stream, StreamExt};
use indexer_allocation::Allocation;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_watcher::watch_pipe;
use prometheus::{register_counter_vec, CounterVec};
use ractor::{
    concurrency::JoinHandle, Actor, ActorCell, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use reqwest::Url;
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use thegraph_core::alloy::{primitives::Address, sol_types::Eip712Domain};
use tokio::{
    select,
    sync::watch::{self, Receiver},
};

use super::sender_account::{
    SenderAccount, SenderAccountArgs, SenderAccountConfig, SenderAccountMessage,
};
use crate::{agent::sender_allocation::SenderAllocationMessage, lazy_static};

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
    ) -> Result<Self::State, ActorProcessingErr> {
        let (allocations_tx, allocations_rx) = watch::channel(HashSet::<Address>::new());
        watch_pipe(indexer_allocations.clone(), move |allocation_id| {
            let allocation_set = allocation_id.keys().cloned().collect::<HashSet<Address>>();
            allocations_tx
                .send(allocation_set)
                .expect("Failed to update indexer_allocations_set channel");
            async {}
        });
        let pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        let myself_clone = myself.clone();
        let accounts_clone = escrow_accounts.clone();
        let _eligible_allocations_senders_handle =
            watch_pipe(accounts_clone, move |escrow_accounts| {
                let senders = escrow_accounts.get_senders();
                myself_clone
                    .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(senders))
                    .unwrap_or_else(|e| {
                        tracing::error!("Error while updating sender_accounts: {:?}", e);
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
            _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
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
            myself.get_cell(),
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
    ) -> Result<(), ActorProcessingErr> {
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
    ) -> Result<(), ActorProcessingErr> {
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
                let Some(sender_id) = sender_id.split(':').next_back() else {
                    tracing::error!(%sender_id, "Could not extract sender_id from name");
                    return Ok(());
                };
                let Ok(sender_id) = Address::parse_checksummed(sender_id, None) else {
                    tracing::error!(%sender_id, "Could not convert sender_id to Address");
                    return Ok(());
                };

                let mut sender_allocation = select! {
                    sender_allocation = state.get_pending_sender_allocation_id() => sender_allocation,
                    _ = tokio::time::sleep(state.config.tap_sender_timeout) => {
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
            tracing::error!(
                "There was an error while starting the sender {}, denying it. Error: {:?}",
                sender_id,
                e
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
    ) -> anyhow::Result<SenderAccountArgs> {
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
    actor_cell: ActorCell,
    mut pglistener: PgListener,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<String>,
) {
    pglistener
        .listen("scalar_tap_receipt_notification")
        .await
        .expect(
            "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
        );
    loop {
        let Ok(pg_notification) = pglistener.recv().await else {
            tracing::error!(
                "should be able to receive Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'"
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

async fn handle_notification(
    new_receipt_notification: NewReceiptNotification,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use ractor::{Actor, ActorRef, ActorStatus};
    use reqwest::Url;
    use ruint::aliases::U256;
    use sqlx::{postgres::PgListener, PgPool};
    use test_assets::{flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
    use thegraph_core::alloy::hex::ToHexExt;
    use tokio::sync::{
        mpsc::{self, error::TryRecvError},
        watch,
    };

    use super::{new_receipts_watcher, SenderAccountsManagerMessage, State};
    use crate::{
        agent::{
            sender_account::{tests::PREFIX_ID, SenderAccountMessage},
            sender_accounts_manager::{handle_notification, NewReceiptNotification},
        },
        test::{
            actors::{DummyActor, MockSenderAccount, MockSenderAllocation, TestableActor},
            create_rav, create_received_receipt, create_sender_accounts_manager, get_config,
            get_grpc_url, store_rav, store_receipt, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER,
            SENDER_2, TAP_EIP712_DOMAIN_SEPARATOR,
        },
    };

    async fn get_subgraph_client() -> &'static SubgraphClient {
        Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&get_grpc_url().await).unwrap(),
            )
            .await,
        ))
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_sender_accounts_manager(pgpool: PgPool) {
        let (_, _, (actor, join_handle)) =
            create_sender_accounts_manager().pgpool(pgpool).call().await;
        actor.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }

    async fn create_state(pgpool: PgPool) -> (String, State) {
        let config = get_config();
        let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
        let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);

        let prefix = format!(
            "test-{}",
            PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        (
            prefix.clone(),
            State {
                config,
                domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
                sender_ids: HashSet::new(),
                new_receipts_watcher_handle: None,
                _eligible_allocations_senders_handle: tokio::spawn(async move {}),
                pgpool,
                indexer_allocations: watch::channel(HashSet::new()).1,
                escrow_accounts: watch::channel(escrow_accounts).1,
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_pending_sender_allocations(pgpool: PgPool) {
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

        let pending_allocation_id = state.get_pending_sender_allocation_id().await;

        // check if pending allocations are correct
        assert_eq!(pending_allocation_id.len(), 1);
        assert!(pending_allocation_id.contains_key(&SENDER.1));
        assert_eq!(pending_allocation_id.get(&SENDER.1).unwrap().len(), 2);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_update_sender_allocation(pgpool: PgPool) {
        let (prefix, notify, (actor, join_handle)) =
            create_sender_accounts_manager().pgpool(pgpool).call().await;

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
                vec![SENDER.1].into_iter().collect(),
            ))
            .unwrap();

        flush_messages(&notify).await;

        // verify if create sender account
        let actor_ref =
            ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix.clone(), SENDER.1));
        assert!(actor_ref.is_some());

        actor
            .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
                HashSet::new(),
            ))
            .unwrap();

        flush_messages(&notify).await;
        // verify if it gets removed
        let actor_ref =
            ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER.1));
        assert!(actor_ref.is_none());

        // safely stop the manager
        actor.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_sender_account(pgpool: PgPool) {
        let (prefix, state) = create_state(pgpool.clone()).await;
        let supervisor = DummyActor::spawn().await;
        // we wait to check if the sender is created
        state
            .create_sender_account(supervisor.get_cell(), SENDER_2.1, HashSet::new())
            .await
            .unwrap();

        let actor_ref =
            ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER_2.1));
        assert!(actor_ref.is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_deny_sender_account_on_failure(pgpool: PgPool) {
        let (_prefix, state) = create_state(pgpool.clone()).await;
        let supervisor = DummyActor::spawn().await;

        state
            .create_or_deny_sender(supervisor.get_cell(), INDEXER.1, HashSet::new())
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_receive_notifications(pgpool: PgPool) {
        let prefix = format!(
            "test-{}",
            PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        // create dummy allocation

        let (mock_sender_allocation, mut receipts) = MockSenderAllocation::new_with_receipts();
        let actor = TestableActor::new(mock_sender_allocation);
        let notify = actor.notify.clone();
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

        // create tokio task to listen for notifications

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
        let new_receipts_watcher_handle = tokio::spawn(new_receipts_watcher(
            dummy_actor.get_cell(),
            pglistener,
            escrow_accounts_rx,
            Some(prefix.clone()),
        ));

        let receipts_count = 10;
        // add receipts to the database
        for i in 1..=receipts_count {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }
        flush_messages(&notify).await;

        // check if receipt notification was sent to the allocation
        for i in 1..=receipts_count {
            let receipt = receipts.recv().await.unwrap();

            assert_eq!(i, receipt.id);
        }
        assert_eq!(receipts.try_recv().unwrap_err(), TryRecvError::Empty);

        new_receipts_watcher_handle.abort();
    }

    #[test_log::test(sqlx::test(migrations = "../../migrations"))]
    async fn test_manager_killed_in_database_connection(pgpool: PgPool) {
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
        let new_receipts_watcher_handle = tokio::spawn(new_receipts_watcher(
            dummy_actor.get_cell(),
            pglistener,
            escrow_accounts_rx,
            None,
        ));
        pgpool.close().await;
        new_receipts_watcher_handle.await.unwrap();

        assert_eq!(dummy_actor.get_status(), ActorStatus::Stopped)
    }

    #[tokio::test]
    async fn test_create_allocation_id() {
        let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
        let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);
        let escrow_accounts = watch::channel(escrow_accounts).1;

        let prefix = format!(
            "test-{}",
            PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );

        let (last_message_emitted, mut rx) = mpsc::channel(64);

        let (sender_account, join_handle) = MockSenderAccount::spawn(
            Some(format!("{}:{}", prefix.clone(), SENDER.1,)),
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

        handle_notification(new_receipt_notification, escrow_accounts, Some(&prefix))
            .await
            .unwrap();

        assert_eq!(
            rx.recv().await.unwrap(),
            SenderAccountMessage::NewAllocationId(ALLOCATION_ID_0)
        );
        sender_account.stop_and_wait(None, None).await.unwrap();
        join_handle.await.unwrap();
    }
}
