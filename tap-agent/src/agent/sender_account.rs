// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use alloy_sol_types::Eip712Domain;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use ractor::{call, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use sqlx::PgPool;
use thegraph::types::Address;
use tracing::error;

use super::sender_allocation::{SenderAllocation, SenderAllocationArgs};
use crate::agent::allocation_id_tracker::AllocationIdTracker;
use crate::agent::sender_allocation::SenderAllocationMessage;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::escrow_adapter::EscrowAdapter,
};

#[derive(Debug)]
pub enum SenderAccountMessage {
    UpdateAllocationIds(HashSet<Address>),
    UpdateReceiptFees(Address, UnaggregatedReceipts),
    #[cfg(test)]
    GetAllocationTracker(ractor::RpcReplyPort<AllocationIdTracker>),
}

/// A SenderAccount manages the receipts accounting between the indexer and the sender across
/// multiple allocations.
///
/// Manages the lifecycle of Scalar TAP for the SenderAccount, including:
/// - Monitoring new receipts and keeping track of the cumulative unaggregated fees across
///   allocations.
/// - Requesting RAVs from the sender's TAP aggregator once the cumulative unaggregated fees reach a
///   certain threshold.
/// - Requesting the last RAV from the sender's TAP aggregator for all EOL allocations.
pub struct SenderAccount;

pub struct SenderAccountArgs {
    pub config: &'static config::Cli,
    pub pgpool: PgPool,
    pub sender_id: Address,
    pub escrow_accounts: Eventual<EscrowAccounts>,
    pub indexer_allocations: Eventual<HashSet<Address>>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub domain_separator: Eip712Domain,
    pub sender_aggregator_endpoint: String,
    pub allocation_ids: HashSet<Address>,
    pub prefix: Option<String>,
}
pub struct State {
    prefix: Option<String>,
    allocation_id_tracker: AllocationIdTracker,
    allocation_ids: HashSet<Address>,
    _indexer_allocations_handle: PipeHandle,
    sender: Address,

    //Eventuals
    escrow_accounts: Eventual<EscrowAccounts>,

    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    domain_separator: Eip712Domain,
    config: &'static config::Cli,
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
}

impl State {
    async fn create_sender_allocation(
        &self,
        sender_account_ref: ActorRef<SenderAccountMessage>,
        allocation_id: Address,
    ) -> Result<()> {
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
        let Some(allocation_id) = self.allocation_id_tracker.get_heaviest_allocation_id() else {
            anyhow::bail!("Error while getting the heaviest allocation because none has unaggregated fees tracked");
        };
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation actor with most unaggregated fees");
        };
        // we call and wait for the response so we don't process anymore update
        let result = call!(allocation, SenderAllocationMessage::TriggerRAVRequest)?;

        self.allocation_id_tracker
            .add_or_update(allocation_id, result.value);
        Ok(())
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
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let clone = myself.clone();
        let _indexer_allocations_handle =
            indexer_allocations
                .clone()
                .pipe_async(move |allocation_ids| {
                    let myself = clone.clone();
                    async move {
                        // Update the allocation_ids
                        myself
                            .cast(SenderAccountMessage::UpdateAllocationIds(allocation_ids))
                            .unwrap_or_else(|e| {
                                error!("Error while updating allocation_ids: {:?}", e);
                            });
                    }
                });

        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone(), sender_id);

        let state = State {
            allocation_id_tracker: AllocationIdTracker::default(),
            allocation_ids: allocation_ids.clone(),
            _indexer_allocations_handle,
            prefix,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            domain_separator,
            sender_aggregator_endpoint,
            config,
            pgpool,
            sender: sender_id,
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
        tracing::trace!(
            sender = %state.sender,
            message = ?message,
            "New SenderAccount message"
        );
        match message {
            SenderAccountMessage::UpdateReceiptFees(allocation_id, unaggregated_fees) => {
                let tracker = &mut state.allocation_id_tracker;
                tracker.add_or_update(allocation_id, unaggregated_fees.value);

                if tracker.get_total_fee() >= state.config.tap.rav_request_trigger_value.into() {
                    state.rav_requester_single().await?;
                }
            }
            SenderAccountMessage::UpdateAllocationIds(allocation_ids) => {
                // Create new sender allocations
                for allocation_id in allocation_ids.difference(&state.allocation_ids) {
                    state
                        .create_sender_allocation(myself.clone(), *allocation_id)
                        .await?;
                }

                // Remove sender allocations
                for allocation_id in state.allocation_ids.difference(&allocation_ids) {
                    if let Some(sender_handle) = ActorRef::<SenderAllocationMessage>::where_is(
                        state.format_sender_allocation(allocation_id),
                    ) {
                        sender_handle.stop(None);
                    }
                }

                state.allocation_ids = allocation_ids;
            }
            #[cfg(test)]
            SenderAccountMessage::GetAllocationTracker(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.allocation_id_tracker.clone());
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

                let tracker = &mut state.allocation_id_tracker;
                tracker.add_or_update(allocation_id, 0);
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

                state
                    .create_sender_allocation(myself.clone(), allocation_id)
                    .await?;
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
        ALLOCATION_ID_0, INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    };
    use alloy_primitives::Address;
    use eventuals::Eventual;
    use indexer_common::escrow_accounts::EscrowAccounts;
    use indexer_common::prelude::{DeploymentDetails, SubgraphClient};
    use ractor::concurrency::JoinHandle;
    use ractor::{Actor, ActorProcessingErr, ActorRef, ActorStatus};
    use sqlx::PgPool;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // we implement the PartialEq and Eq traits for SenderAccountMessage to be able to compare
    impl Eq for SenderAccountMessage {}

    impl PartialEq for SenderAccountMessage {
        fn eq(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::UpdateAllocationIds(l0), Self::UpdateAllocationIds(r0)) => l0 == r0,
                (Self::UpdateReceiptFees(l0, l1), Self::UpdateReceiptFees(r0, r1)) => {
                    l0 == r0 && l1 == r1
                }
                _ => core::mem::discriminant(self) == core::mem::discriminant(other),
            }
        }
    }

    pub static PREFIX_ID: AtomicU32 = AtomicU32::new(0);
    const DUMMY_URL: &str = "http://localhost:1234";
    const TRIGGER_VALUE: u128 = 500;

    async fn create_sender_account(
        pgpool: PgPool,
        initial_allocation: HashSet<Address>,
    ) -> (
        ActorRef<SenderAccountMessage>,
        tokio::task::JoinHandle<()>,
        String,
    ) {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: TRIGGER_VALUE as u64,
                rav_request_timestamp_buffer_ms: 1,
                rav_request_timeout_secs: 5,
                ..Default::default()
            },
            ..Default::default()
        }));

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(DUMMY_URL).unwrap(),
        )));

        let escrow_accounts_eventual = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
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
        };

        let (sender, handle) = SenderAccount::spawn(Some(prefix.clone()), SenderAccount, args)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        (sender, handle, prefix)
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_allocation_ids(pgpool: PgPool) {
        let (sender_account, handle, prefix) = create_sender_account(pgpool, HashSet::new()).await;

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

    pub struct MockSenderAllocation {
        triggered_rav_request: Arc<AtomicBool>,
        receipts: Arc<Mutex<Vec<NewReceiptNotification>>>,
    }

    impl MockSenderAllocation {
        pub fn new_with_triggered_rav_request() -> (Self, Arc<AtomicBool>) {
            let triggered_rav_request = Arc::new(AtomicBool::new(false));
            (
                Self {
                    triggered_rav_request: triggered_rav_request.clone(),
                    receipts: Arc::new(Mutex::new(Vec::new())),
                },
                triggered_rav_request,
            )
        }

        pub fn new_with_receipts() -> (Self, Arc<Mutex<Vec<NewReceiptNotification>>>) {
            let receipts = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    triggered_rav_request: Arc::new(AtomicBool::new(false)),
                    receipts: receipts.clone(),
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
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    reply.send(UnaggregatedReceipts::default())?;
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
        Arc<AtomicBool>,
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
        let (sender_account, handle, prefix) = create_sender_account(pgpool, HashSet::new()).await;

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

        assert!(!triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst));

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_receipt_fees_trigger_rav(pgpool: PgPool) {
        let (sender_account, handle, prefix) = create_sender_account(pgpool, HashSet::new()).await;

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

        assert!(triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst));

        allocation.stop_and_wait(None, None).await.unwrap();
        allocation_handle.await.unwrap();

        sender_account.stop_and_wait(None, None).await.unwrap();
        handle.await.unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_remove_sender_account(pgpool: PgPool) {
        let (sender_account, handle, prefix) =
            create_sender_account(pgpool, vec![*ALLOCATION_ID_0].into_iter().collect()).await;

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
}
