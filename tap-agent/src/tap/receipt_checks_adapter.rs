// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc, time::Duration};

use alloy_primitives::Address;
use async_trait::async_trait;
use ethereum_types::U256;
use eventuals::{timer, Eventual, EventualExt};
use indexer_common::subgraph_client::SubgraphClient;
use serde_json::json;
use sqlx::PgPool;
use tap_core::adapters::receipt_checks_adapter::ReceiptChecksAdapter as ReceiptChecksAdapterTrait;
use tap_core::{eip_712_signed_message::EIP712SignedMessage, tap_receipt::Receipt};
use thiserror::Error;
use tokio::{sync::RwLock, time::sleep};
use tracing::error;

use crate::config;

pub struct ReceiptChecksAdapter {
    query_appraisals: Option<Arc<RwLock<HashMap<u64, u128>>>>,
    allocation_id: Address,
    escrow_accounts: Eventual<HashMap<Address, U256>>,
    tap_allocation_redeemed: Eventual<bool>,
}

impl ReceiptChecksAdapter {
    pub fn new(
        config: &'static config::Cli,
        _pgpool: PgPool,
        query_appraisals: Option<Arc<RwLock<HashMap<u64, u128>>>>,
        allocation_id: Address,
        escrow_accounts: Eventual<HashMap<Address, U256>>,
        escrow_subgraph: &'static SubgraphClient,
        sender_id: Address,
    ) -> Self {
        let tap_allocation_redeemed = Self::tap_allocation_redeemed_eventual(
            allocation_id,
            sender_id,
            config.ethereum.indexer_address,
            escrow_subgraph,
            config.escrow_subgraph.escrow_syncing_interval,
        );
        Self {
            query_appraisals,
            allocation_id,
            escrow_accounts,
            tap_allocation_redeemed,
        }
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Error in ReceiptChecksAdapter: {error}")]
    AdapterError { error: String },
}

#[async_trait]
impl ReceiptChecksAdapterTrait for ReceiptChecksAdapter {
    type AdapterError = AdapterError;

    /// Should not be called. Will panic if called. This method is unneeded for the
    /// moment.
    ///
    /// Since we don't do on the fly receipt checking, we don't need to check for
    /// uniqueness with this method. The uniqueness check will be carried out by
    /// `tap_core::tap_manager::Manager` on the basis of the receipts to be aggregated.
    async fn is_unique(
        &self,
        _receipt: &EIP712SignedMessage<Receipt>,
        _receipt_id: u64,
    ) -> Result<bool, Self::AdapterError> {
        unimplemented!();
    }

    async fn is_valid_allocation_id(
        &self,
        allocation_id: Address,
    ) -> Result<bool, Self::AdapterError> {
        // TODO: Remove the if block below? Each TAP Monitor is specific to an allocation
        // ID. So the receipts that are received here should already have been filtered by
        // allocation ID.
        if allocation_id != self.allocation_id {
            return Ok(false);
        };

        // Check that the allocation ID is not redeemed yet for this consumer
        match self.tap_allocation_redeemed.value().await {
            Ok(redeemed) => Ok(!redeemed),
            Err(e) => Err(AdapterError::AdapterError {
                error: format!(
                    "Could not get allocation escrow redemption status from eventual: {:?}",
                    e
                ),
            }),
        }
    }

    async fn is_valid_value(&self, value: u128, query_id: u64) -> Result<bool, Self::AdapterError> {
        let query_appraisals = self.query_appraisals.as_ref().expect(
            "Query appraisals should be initialized. The opposite should never happen when \
            receipts value checking is enabled.",
        );
        let query_appraisals_read = query_appraisals.read().await;
        let appraised_value =
            query_appraisals_read
                .get(&query_id)
                .ok_or_else(|| AdapterError::AdapterError {
                    error: "No appraised value found for query".to_string(),
                })?;
        if value != *appraised_value {
            return Ok(false);
        }
        Ok(true)
    }

    async fn is_valid_gateway_id(&self, gateway_id: Address) -> Result<bool, Self::AdapterError> {
        let escrow_accounts =
            self.escrow_accounts
                .value()
                .await
                .map_err(|e| AdapterError::AdapterError {
                    error: format!("Could not get escrow accounts from eventual: {:?}", e),
                })?;

        Ok(escrow_accounts
            .get(&gateway_id)
            .map_or(false, |balance| *balance > U256::from(0)))
    }
}

impl ReceiptChecksAdapter {
    fn tap_allocation_redeemed_eventual(
        allocation_id: Address,
        sender_address: Address,
        indexer_address: Address,
        escrow_subgraph: &'static SubgraphClient,
        escrow_subgraph_polling_interval: u64,
    ) -> Eventual<bool> {
        #[derive(serde::Deserialize)]
        struct AllocationResponse {
            #[allow(dead_code)]
            id: String,
        }

        #[derive(serde::Deserialize)]
        struct TransactionsResponse {
            transactions: Vec<AllocationResponse>,
        }

        timer(Duration::from_secs(escrow_subgraph_polling_interval)).map_with_retry(
            move |_| async move {
                let response = escrow_subgraph
                    .query::<TransactionsResponse>(&json!({
                        "query": r#"
                                    query (
                                        $sender_id: ID!, 
                                        $receiver_id: ID!, 
                                        $allocation_id: String!
                                    ) {
                                        transactions(
                                            where: {
                                                and: [
                                                    { type: "redeem" }
                                                    { sender_: { id: $sender_id } }
                                                    { receiver_: { id: $receiver_id } }
                                                    { allocationID: $allocation_id }
                                                ]
                                            }
                                        ) {
                                            allocationID
                                            sender {
                                                id
                                            }
                                        }
                                    }
                                "#,
                        "variables": {
                            "sender_id": sender_address.to_string(),
                            "receiver_id": indexer_address.to_string(),
                            "allocation_id": allocation_id.to_string(),
                        }
                    }))
                    .await
                    .map_err(|e| e.to_string())?;
                let response = response.data.ok_or_else(|| {
                    format!(
                        "No data found in escrow subgraph response for allocation {} and sender {}",
                        allocation_id, sender_address
                    )
                })?;
                Ok(!response.transactions.is_empty())
            },
            move |error: String| {
                error!(
                    "Failed to check the escrow redeem status for allocation {} and sender {}: {}",
                    allocation_id, sender_address, error
                );
                sleep(Duration::from_secs(escrow_subgraph_polling_interval).div_f32(2.))
            },
        )
    }
}
