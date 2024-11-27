// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{
    dyn_abi::Eip712Domain,
    hex::ToHexExt,
    primitives::{Address, U256},
};
use anyhow::Result;
use bigdecimal::ToPrimitive;
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use indexer_query::closed_allocations::{self, ClosedAllocations};
use ractor::{Actor, ActorRef, MessagingErr};
use sqlx::PgPool;
use std::{collections::HashSet, str::FromStr, time::Duration};
use tap_core::rav::SignedRAV;
use tokio::{sync::watch::Receiver, task::JoinHandle};
use tracing::error;

use crate::{
    adaptative_concurrency::AdaptiveLimiter,
    agent::{
        sender_account::{SenderAccount, SENDER_DENIED},
        sender_allocation::{
            AllocationConfig, SenderAllocation, SenderAllocationArgs, SenderAllocationMessage,
        },
        unaggregated_receipts::UnaggregatedReceipts,
    },
    backoff::BackoffInfo,
    tracker::{SenderFeeTracker, SimpleFeeTracker},
};

use super::{
    SenderAccountConfig, SenderAccountMessage, PENDING_RAV, SENDER_FEE_TRACKER, UNAGGREGATED_FEES,
};

pub struct State {
    pub(super) prefix: Option<String>,
    pub(super) sender_fee_tracker: SenderFeeTracker,
    pub(super) rav_tracker: SimpleFeeTracker,
    pub(super) invalid_receipts_tracker: SimpleFeeTracker,
    pub(super) allocation_ids: HashSet<Address>,
    pub(super) _indexer_allocations_handle: JoinHandle<()>,
    pub(super) _escrow_account_monitor: JoinHandle<()>,
    pub(super) scheduled_rav_request:
        Option<JoinHandle<Result<(), MessagingErr<SenderAccountMessage>>>>,

    pub(super) sender: Address,

    // Deny reasons
    pub(super) denied: bool,
    pub(super) sender_balance: U256,
    pub(super) retry_interval: Duration,

    // concurrent rav request
    pub(super) adaptive_limiter: AdaptiveLimiter,

    // Receivers
    pub(super) escrow_accounts: Receiver<EscrowAccounts>,

    pub(super) escrow_subgraph: &'static SubgraphClient,
    pub(super) network_subgraph: &'static SubgraphClient,

    pub(super) domain_separator: Eip712Domain,
    pub(super) pgpool: PgPool,
    pub(super) sender_aggregator: jsonrpsee::http_client::HttpClient,

    // Backoff info
    pub(super) backoff_info: BackoffInfo,

    // Config
    pub(super) config: &'static SenderAccountConfig,
}

impl State {
    pub(super) async fn create_sender_allocation(
        &self,
        sender_account_ref: ActorRef<SenderAccountMessage>,
        allocation_id: Address,
    ) -> Result<()> {
        tracing::trace!(
            %self.sender,
            %allocation_id,
            "SenderAccount is creating allocation."
        );
        let args = SenderAllocationArgs {
            pgpool: self.pgpool.clone(),
            allocation_id,
            sender: self.sender,
            escrow_accounts: self.escrow_accounts.clone(),
            escrow_subgraph: self.escrow_subgraph,
            domain_separator: self.domain_separator.clone(),
            sender_account_ref: sender_account_ref.clone(),
            sender_aggregator: self.sender_aggregator.clone(),
            config: AllocationConfig::from_sender_config(self.config),
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
    pub(super) fn format_sender_allocation(&self, allocation_id: &Address) -> String {
        let mut sender_allocation_id = String::new();
        if let Some(prefix) = &self.prefix {
            sender_allocation_id.push_str(prefix);
            sender_allocation_id.push(':');
        }
        sender_allocation_id.push_str(&format!("{}:{}", self.sender, allocation_id));
        sender_allocation_id
    }

    pub(super) async fn rav_request_for_heaviest_allocation(&mut self) -> Result<()> {
        let allocation_id = self
            .sender_fee_tracker
            .get_heaviest_allocation_id()
            .ok_or_else(|| {
                self.backoff_info.fail();
                anyhow::anyhow!(
                    "Error while getting the heaviest allocation, \
            this is due one of the following reasons: \n
            1. allocations have too much fees under their buffer\n
            2. allocations are blocked to be redeemed due to ongoing last rav. \n
            If you keep seeing this message try to increase your `amount_willing_to_lose` \
            and restart your `tap-agent`\n
            If this doesn't work, open an issue on our Github."
                )
            })?;
        self.backoff_info.ok();
        self.rav_request_for_allocation(allocation_id).await
    }

    pub(super) async fn rav_request_for_allocation(
        &mut self,
        allocation_id: Address,
    ) -> Result<()> {
        let sender_allocation_id = self.format_sender_allocation(&allocation_id);
        let allocation = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id);

        let Some(allocation) = allocation else {
            anyhow::bail!("Error while getting allocation actor {allocation_id}");
        };

        allocation
            .cast(SenderAllocationMessage::TriggerRAVRequest)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Error while sending and waiting message for actor {allocation_id}. Error: {e}"
                )
            })?;
        self.adaptive_limiter.acquire();
        self.sender_fee_tracker.start_rav_request(allocation_id);

        Ok(())
    }

    pub(super) fn finalize_rav_request(
        &mut self,
        allocation_id: Address,
        rav_response: (UnaggregatedReceipts, anyhow::Result<Option<SignedRAV>>),
    ) {
        self.sender_fee_tracker.finish_rav_request(allocation_id);
        let (fees, rav_result) = rav_response;
        match rav_result {
            Ok(signed_rav) => {
                self.sender_fee_tracker.ok_rav_request(allocation_id);
                self.adaptive_limiter.on_success();
                let rav_value = signed_rav.map_or(0, |rav| rav.message.valueAggregate);
                self.update_rav(allocation_id, rav_value);
            }
            Err(err) => {
                self.sender_fee_tracker.failed_rav_backoff(allocation_id);
                self.adaptive_limiter.on_failure();
                error!(
                    "Error while requesting RAV for sender {} and allocation {}: {}",
                    self.sender, allocation_id, err
                );
            }
        };
        self.update_sender_fee(allocation_id, fees);
    }

    pub(super) fn update_rav(&mut self, allocation_id: Address, rav_value: u128) {
        self.rav_tracker.update(allocation_id, rav_value);
        PENDING_RAV
            .with_label_values(&[&self.sender.to_string(), &allocation_id.to_string()])
            .set(rav_value as f64);
    }

    pub(super) fn update_sender_fee(
        &mut self,
        allocation_id: Address,
        unaggregated_fees: UnaggregatedReceipts,
    ) {
        self.sender_fee_tracker
            .update(allocation_id, unaggregated_fees);
        SENDER_FEE_TRACKER
            .with_label_values(&[&self.sender.to_string()])
            .set(self.sender_fee_tracker.get_total_fee() as f64);

        UNAGGREGATED_FEES
            .with_label_values(&[&self.sender.to_string(), &allocation_id.to_string()])
            .set(unaggregated_fees.value as f64);
    }

    pub(super) fn deny_condition_reached(&self) -> bool {
        let pending_ravs = self.rav_tracker.get_total_fee();
        let unaggregated_fees = self.sender_fee_tracker.get_total_fee();
        let pending_fees_over_balance =
            U256::from(pending_ravs + unaggregated_fees) >= self.sender_balance;
        let max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt;
        let invalid_receipt_fees = self.invalid_receipts_tracker.get_total_fee();
        let total_fee_over_max_value =
            unaggregated_fees + invalid_receipt_fees >= max_amount_willing_to_lose;

        tracing::trace!(
            %pending_fees_over_balance,
            %total_fee_over_max_value,
            "Verifying if deny condition was reached.",
        );

        total_fee_over_max_value || pending_fees_over_balance
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    pub(super) async fn add_to_denylist(&mut self) {
        tracing::warn!(
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt,
            sender_balance = self.sender_balance.to_u128(),
            "Denying sender."
        );

        SenderAccount::deny_sender(&self.pgpool, self.sender).await;
        self.denied = true;
        SENDER_DENIED
            .with_label_values(&[&self.sender.to_string()])
            .set(1);
    }

    /// Will update [`State::denied`], as well as the denylist table in the database.
    pub(super) async fn remove_from_denylist(&mut self) {
        tracing::info!(
            fee_tracker = self.sender_fee_tracker.get_total_fee(),
            rav_tracker = self.rav_tracker.get_total_fee(),
            max_amount_willing_to_lose = self.config.max_amount_willing_to_lose_grt,
            sender_balance = self.sender_balance.to_u128(),
            "Allowing sender."
        );
        sqlx::query!(
            r#"
                    DELETE FROM scalar_tap_denylist
                    WHERE sender_address = $1
                "#,
            self.sender.encode_hex(),
        )
        .execute(&self.pgpool)
        .await
        .expect("Should not fail to delete from denylist");
        self.denied = false;

        SENDER_DENIED
            .with_label_values(&[&self.sender.to_string()])
            .set(0);
    }

    /// receives a list of possible closed allocations and verify
    /// if they are really closed
    pub(super) async fn check_closed_allocations(
        &self,
        allocation_ids: HashSet<&Address>,
    ) -> anyhow::Result<HashSet<Address>> {
        if allocation_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let allocation_ids: Vec<String> = allocation_ids
            .into_iter()
            .map(|addr| addr.to_string().to_lowercase())
            .collect();

        let mut hash: Option<String> = None;
        let mut last: Option<String> = None;
        let mut responses = vec![];
        let page_size = 200;

        loop {
            let result = self
                .network_subgraph
                .query::<ClosedAllocations, _>(closed_allocations::Variables {
                    allocation_ids: allocation_ids.clone(),
                    first: page_size,
                    last: last.unwrap_or_default(),
                    block: hash.map(|hash| closed_allocations::Block_height {
                        hash: Some(hash),
                        number: None,
                        number_gte: None,
                    }),
                })
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            let mut data = result?;
            let page_len = data.allocations.len();

            hash = data.meta.and_then(|meta| meta.block.hash);
            last = data.allocations.last().map(|entry| entry.id.to_string());

            responses.append(&mut data.allocations);
            if (page_len as i64) < page_size {
                break;
            }
        }
        Ok(responses
            .into_iter()
            .map(|allocation| Address::from_str(&allocation.id))
            .collect::<Result<HashSet<Address>, _>>()?)
    }
}
