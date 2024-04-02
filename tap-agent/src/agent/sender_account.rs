// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use alloy_sol_types::Eip712Domain;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use ractor::{call, Actor, ActorProcessingErr, ActorRef};
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
    CreateSenderAllocation(Address),
    UpdateAllocationIds(HashSet<Address>),
    RemoveSenderAccount,
    UpdateReceiptFees(Address, UnaggregatedReceipts),
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
            anyhow::bail!("Error while getting allocation with most unaggregated fees");
        };
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation with most unaggregated fees");
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

        for allocation_id in &allocation_ids {
            // Create a sender allocation for each allocation
            myself.cast(SenderAccountMessage::CreateSenderAllocation(*allocation_id))?;
        }

        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone(), sender_id);

        Ok(State {
            allocation_id_tracker: AllocationIdTracker::default(),
            allocation_ids,
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
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match message {
            SenderAccountMessage::RemoveSenderAccount => myself.stop(None),
            SenderAccountMessage::UpdateReceiptFees(allocation_id, unaggregated_fees) => {
                let tracker = &mut state.allocation_id_tracker;
                tracker.add_or_update(allocation_id, unaggregated_fees.value);

                if tracker.get_total_fee() >= state.config.tap.rav_request_trigger_value.into() {
                    state.rav_requester_single().await?;
                }
            }
            SenderAccountMessage::CreateSenderAllocation(allocation_id) => {
                let args = SenderAllocationArgs {
                    config: state.config,
                    pgpool: state.pgpool.clone(),
                    allocation_id,
                    sender: state.sender,
                    escrow_accounts: state.escrow_accounts.clone(),
                    escrow_subgraph: state.escrow_subgraph,
                    escrow_adapter: state.escrow_adapter.clone(),
                    domain_separator: state.domain_separator.clone(),
                    sender_aggregator_endpoint: state.sender_aggregator_endpoint.clone(),
                    sender_account_ref: myself.clone(),
                };

                SenderAllocation::spawn_linked(
                    Some(state.format_sender_allocation(&allocation_id)),
                    SenderAllocation,
                    args,
                    myself.get_cell(),
                )
                .await?;
            }
            SenderAccountMessage::UpdateAllocationIds(allocation_ids) => {
                // Create new sender allocations
                for allocation_id in allocation_ids.difference(&state.allocation_ids) {
                    myself.cast(SenderAccountMessage::CreateSenderAllocation(*allocation_id))?;
                }

                // Remove sender allocations
                for allocation_id in state.allocation_ids.difference(&allocation_ids) {
                    if let Some(sender_handle) = ActorRef::<SenderAllocationMessage>::where_is(
                        state.format_sender_allocation(allocation_id),
                    ) {
                        sender_handle.cast(SenderAllocationMessage::CloseAllocation)?;
                    }
                }

                state.allocation_ids = allocation_ids;
            }
            // #[cfg(test)]
            SenderAccountMessage::GetAllocationTracker(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(state.allocation_id_tracker.clone());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SenderAccountMessage;

    // we implement the PartialEq and Eq traits for SenderAccountMessage to be able to compare
    impl Eq for SenderAccountMessage {}

    impl PartialEq for SenderAccountMessage {
        fn eq(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::CreateSenderAllocation(l0), Self::CreateSenderAllocation(r0)) => l0 == r0,
                (Self::UpdateAllocationIds(l0), Self::UpdateAllocationIds(r0)) => l0 == r0,
                (Self::UpdateReceiptFees(l0, l1), Self::UpdateReceiptFees(r0, r1)) => {
                    l0 == r0 && l1 == r1
                }
                _ => core::mem::discriminant(self) == core::mem::discriminant(other),
            }
        }
    }

    #[test]
    fn test_update_allocation_ids() {}

    #[test]
    fn test_update_receipt_fees_no_rav() {}

    #[test]
    fn test_update_receipt_fees_trigger_rav() {}

    #[test]
    fn test_remove_sender_account() {}

    #[test]
    fn test_create_sender_allocation() {}

    #[test]
    fn test_create_sender_account() {}
}
