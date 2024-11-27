// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use alloy::{dyn_abi::Eip712Domain, primitives::Address};
use anyhow::anyhow;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use sqlx::PgPool;
use state::SenderAllocationState;
use tokio::sync::watch::Receiver;
use tracing::{error, warn};
use typed_builder::TypedBuilder;

use crate::{
    agent::{
        metrics::CLOSED_SENDER_ALLOCATIONS,
        sender_account::{ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::NewReceiptNotification,
    },
    tap::context::TapAgentContext,
};
use thiserror::Error;

use super::sender_account::SenderAccountConfig;

mod state;
#[cfg(test)]
pub mod tests;

#[derive(Error, Debug)]
pub enum RavError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    TapCore(#[from] tap_core::Error),

    #[error(transparent)]
    JsonRpsee(#[from] jsonrpsee::core::ClientError),

    #[error("All receipts are invalid")]
    AllReceiptsInvalid,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type TapManager = tap_core::manager::Manager<TapAgentContext>;

/// Manages unaggregated fees and the TAP lifecyle for a specific (allocation, sender) pair.
pub struct SenderAllocation;

#[derive(Clone)]
pub struct AllocationConfig {
    pub timestamp_buffer_ns: u64,
    pub rav_request_receipt_limit: u64,
    pub indexer_address: Address,
    pub escrow_polling_interval: Duration,
}

impl AllocationConfig {
    pub fn from_sender_config(config: &SenderAccountConfig) -> Self {
        Self {
            timestamp_buffer_ns: config.rav_request_buffer.as_nanos() as u64,
            rav_request_receipt_limit: config.rav_request_receipt_limit,
            indexer_address: config.indexer_address,
            escrow_polling_interval: config.escrow_polling_interval,
        }
    }
}

#[derive(TypedBuilder)]
pub struct SenderAllocationArgs {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    domain_separator: Eip712Domain,
    sender_account_ref: ActorRef<SenderAccountMessage>,
    sender_aggregator: jsonrpsee::http_client::HttpClient,

    //config
    config: AllocationConfig,
}

#[derive(Debug)]
pub enum SenderAllocationMessage {
    NewReceipt(NewReceiptNotification),
    TriggerRAVRequest,
    #[cfg(test)]
    GetUnaggregatedReceipts(
        ractor::RpcReplyPort<super::unaggregated_receipts::UnaggregatedReceipts>,
    ),
}

#[async_trait::async_trait]
impl Actor for SenderAllocation {
    type Msg = SenderAllocationMessage;
    type State = SenderAllocationState;
    type Arguments = SenderAllocationArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let sender_account_ref = args.sender_account_ref.clone();
        let allocation_id = args.allocation_id;
        let mut state = SenderAllocationState::new(args).await?;

        // update invalid receipts
        state.invalid_receipts_fees = state.calculate_invalid_receipts_fee().await?;
        if state.invalid_receipts_fees.value > 0 {
            sender_account_ref.cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                allocation_id,
                state.invalid_receipts_fees,
            ))?;
        }

        // update unaggregated_fees
        state.unaggregated_fees = state.recalculate_all_unaggregated_fees().await?;

        sender_account_ref.cast(SenderAccountMessage::UpdateReceiptFees(
            allocation_id,
            ReceiptFees::UpdateValue(state.unaggregated_fees),
        ))?;

        // update rav tracker for sender account
        if let Some(rav) = &state.latest_rav {
            sender_account_ref.cast(SenderAccountMessage::UpdateRav(rav.clone()))?;
        }

        tracing::info!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            "SenderAllocation created!",
        );

        Ok(state)
    }

    // this method only runs on graceful stop (real close allocation)
    // if the actor crashes, this is not ran
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        tracing::info!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            "Closing SenderAllocation, triggering last rav",
        );
        loop {
            match state.recalculate_all_unaggregated_fees().await {
                Ok(value) => {
                    state.unaggregated_fees = value;
                    break;
                }
                Err(err) => {
                    error!(
                        error = %err,
                        "There was an error while calculating the last unaggregated receipts. Retrying in 30 seconds...");
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        }
        // Request a RAV and mark the allocation as final.
        while state.unaggregated_fees.value > 0 {
            if let Err(err) = state.request_rav().await {
                error!(error = %err, "There was an error while requesting rav. Retrying in 30 seconds...");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }

        while let Err(err) = state.mark_rav_last().await {
            error!(
                error = %err,
                %state.allocation_id,
                %state.sender,
                "Error while marking allocation last. Retrying in 30 seconds..."
            );
            tokio::time::sleep(Duration::from_secs(30)).await;
        }

        // Since this is only triggered after allocation is closed will be counted here
        CLOSED_SENDER_ALLOCATIONS
            .with_label_values(&[&state.sender.to_string()])
            .inc();

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        tracing::trace!(
            sender = %state.sender,
            allocation_id = %state.allocation_id,
            ?message,
            "New SenderAllocation message"
        );
        let unaggregated_fees = &mut state.unaggregated_fees;
        match message {
            SenderAllocationMessage::NewReceipt(notification) => {
                let NewReceiptNotification {
                    id,
                    value: fees,
                    timestamp_ns,
                    ..
                } = notification;
                if id <= unaggregated_fees.last_id {
                    // our world assumption is wrong
                    warn!(
                        last_id = %id,
                        "Received a receipt notification that was already calculated."
                    );
                    return Ok(());
                }
                unaggregated_fees.last_id = id;
                unaggregated_fees.value =
                    unaggregated_fees
                        .value
                        .checked_add(fees)
                        .unwrap_or_else(|| {
                            // This should never happen, but if it does, we want to know about it.
                            error!(
                            "Overflow when adding receipt value {} to total unaggregated fees {} \
                            for allocation {} and sender {}. Setting total unaggregated fees to \
                            u128::MAX.",
                            fees, unaggregated_fees.value, state.allocation_id, state.sender
                        );
                            u128::MAX
                        });
                unaggregated_fees.counter += 1;
                // it's fine to crash the actor, could not send a message to its parent
                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        state.allocation_id,
                        ReceiptFees::NewReceipt(fees, timestamp_ns),
                    ))?;
            }
            SenderAllocationMessage::TriggerRAVRequest => {
                let rav_result = if state.unaggregated_fees.value > 0 {
                    state.request_rav().await.map(|_| state.latest_rav.clone())
                } else {
                    Err(anyhow!("Unaggregated fee equals zero"))
                };
                let rav_response = (state.unaggregated_fees, rav_result);
                // encapsulate inanother okay, unwrap after and send the result over here
                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        state.allocation_id,
                        ReceiptFees::RavRequestResponse(rav_response),
                    ))?;
            }
            #[cfg(test)]
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(*unaggregated_fees);
                }
            }
        }

        Ok(())
    }
}
