// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use alloy_primitives::hex::ToHex;
use alloy_sol_types::Eip712Domain;
use anyhow::{anyhow, ensure, Result};
use bigdecimal::num_bigint::BigInt;
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::{
    rav::{RAVRequest, ReceiptAggregateVoucher},
    receipt::{
        checks::{Check, Checks},
        Failed, ReceiptWithState,
    },
    signed_message::EIP712SignedMessage,
};
use thegraph::types::Address;
use tracing::{error, warn};

use crate::agent::sender_account::SenderAccountMessage;
use crate::agent::sender_accounts_manager::NewReceiptNotification;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::context::{checks::Signature, TapAgentContext},
    tap::signers_trimmed,
    tap::{context::checks::AllocationId, escrow_adapter::EscrowAdapter},
};

type TapManager = tap_core::manager::Manager<TapAgentContext>;

/// Manages unaggregated fees and the TAP lifecyle for a specific (allocation, sender) pair.
pub struct SenderAllocation;

pub struct SenderAllocationState {
    unaggregated_fees: UnaggregatedReceipts,
    pgpool: PgPool,
    tap_manager: TapManager,
    allocation_id: Address,
    sender: Address,
    sender_aggregator_endpoint: String,
    config: &'static config::Cli,
    escrow_accounts: Eventual<EscrowAccounts>,
    domain_separator: Eip712Domain,
    sender_account_ref: ActorRef<SenderAccountMessage>,
}

pub struct SenderAllocationArgs {
    pub config: &'static config::Cli,
    pub pgpool: PgPool,
    pub allocation_id: Address,
    pub sender: Address,
    pub escrow_accounts: Eventual<EscrowAccounts>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub escrow_adapter: EscrowAdapter,
    pub domain_separator: Eip712Domain,
    pub sender_aggregator_endpoint: String,
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
}

pub enum SenderAllocationMessage {
    NewReceipt(NewReceiptNotification),
    TriggerRAVRequest(RpcReplyPort<UnaggregatedReceipts>),
    CloseAllocation,
    GetUnaggregatedReceipts(RpcReplyPort<UnaggregatedReceipts>),
}

#[async_trait::async_trait]
impl Actor for SenderAllocation {
    type Msg = SenderAllocationMessage;
    type State = SenderAllocationState;
    type Arguments = SenderAllocationArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        SenderAllocationArgs {
            config,
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            domain_separator,
            sender_aggregator_endpoint,
            sender_account_ref,
        }: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let required_checks: Vec<Arc<dyn Check + Send + Sync>> = vec![
            Arc::new(AllocationId::new(
                sender,
                allocation_id,
                escrow_subgraph,
                config,
            )),
            Arc::new(Signature::new(
                domain_separator.clone(),
                escrow_accounts.clone(),
            )),
        ];
        let context = TapAgentContext::new(
            pgpool.clone(),
            allocation_id,
            sender,
            escrow_accounts.clone(),
            escrow_adapter,
        );
        let tap_manager = TapManager::new(
            domain_separator.clone(),
            context,
            Checks::new(required_checks),
        );

        let mut state = SenderAllocationState {
            pgpool,
            tap_manager,
            allocation_id,
            sender,
            sender_aggregator_endpoint,
            config,
            escrow_accounts,
            domain_separator,
            sender_account_ref: sender_account_ref.clone(),
            unaggregated_fees: UnaggregatedReceipts::default(),
        };

        state.unaggregated_fees = state.calculate_unaggregated_fee().await?;
        sender_account_ref.cast(SenderAccountMessage::UpdateReceiptFees(
            allocation_id,
            state.unaggregated_fees.clone(),
        ))?;

        Ok(state)
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        // cleanup receipt fees for sender
        state
            .sender_account_ref
            .cast(SenderAccountMessage::UpdateReceiptFees(
                state.allocation_id,
                UnaggregatedReceipts::default(),
            ))?;
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        let unaggreated_fees = &mut state.unaggregated_fees;
        match message {
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id, value: fees, ..
            }) => {
                if id > unaggreated_fees.last_id {
                    unaggreated_fees.last_id = id;
                    unaggreated_fees.value =
                        unaggreated_fees.value.checked_add(fees).unwrap_or_else(|| {
                            // This should never happen, but if it does, we want to know about it.
                            error!(
                            "Overflow when adding receipt value {} to total unaggregated fees {} \
                            for allocation {} and sender {}. Setting total unaggregated fees to \
                            u128::MAX.",
                            fees, unaggreated_fees.value, state.allocation_id, state.sender
                        );
                            u128::MAX
                        });
                    state
                        .sender_account_ref
                        .cast(SenderAccountMessage::UpdateReceiptFees(
                            state.allocation_id,
                            unaggreated_fees.clone(),
                        ))?;
                }
            }
            SenderAllocationMessage::TriggerRAVRequest(reply) => {
                state.rav_requester_single().await.map_err(|e| {
                    anyhow! {
                        "Error while requesting RAV for sender {} and allocation {}: {}",
                        state.sender,
                        state.allocation_id,
                        e
                    }
                })?;
                state.unaggregated_fees = state.calculate_unaggregated_fee().await?;
                if !reply.is_closed() {
                    let _ = reply.send(state.unaggregated_fees.clone());
                }
            }

            SenderAllocationMessage::CloseAllocation => {
                state.rav_requester_single().await.inspect_err(|e| {
                    error!(
                        "Error while requesting RAV for sender {} and allocation {}: {}",
                        state.sender, state.allocation_id, e
                    );
                })?;
                state.mark_rav_final().await.inspect_err(|e| {
                    error!(
                        "Error while marking allocation {} as final for sender {}: {}",
                        state.allocation_id, state.sender, e
                    );
                })?;
                myself.stop(None);
            }
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(unaggreated_fees.clone());
                }
            }
        }
        Ok(())
    }
}

impl SenderAllocationState {
    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_unaggregated_fee(&self) -> Result<UnaggregatedReceipts> {
        self.tap_manager.remove_obsolete_receipts().await?;

        let signers = signers_trimmed(&self.escrow_accounts, self.sender).await?;

        // TODO: Get `rav.timestamp_ns` from the TAP Manager's RAV storage adapter instead?
        let res = sqlx::query!(
            r#"
            WITH rav AS (
                SELECT 
                    timestamp_ns 
                FROM 
                    scalar_tap_ravs 
                WHERE 
                    allocation_id = $1 
                    AND sender_address = $2
            ) 
            SELECT 
                MAX(id), 
                SUM(value) 
            FROM 
                scalar_tap_receipts 
            WHERE 
                allocation_id = $1 
                AND signer_address IN (SELECT unnest($3::text[]))
                AND CASE WHEN (
                    SELECT 
                        timestamp_ns :: NUMERIC 
                    FROM 
                        rav
                ) IS NOT NULL THEN timestamp_ns > (
                    SELECT 
                        timestamp_ns :: NUMERIC 
                    FROM 
                        rav
                ) ELSE TRUE END
            "#,
            self.allocation_id.encode_hex::<String>(),
            self.sender.encode_hex::<String>(),
            &signers
        )
        .fetch_one(&self.pgpool)
        .await?;

        ensure!(
            res.sum.is_none() == res.max.is_none(),
            "Exactly one of SUM(value) and MAX(id) is null. This should not happen."
        );

        Ok(UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
        })
    }

    /// Request a RAV from the sender's TAP aggregator. Only one RAV request will be running at a
    /// time through the use of an internal guard.
    async fn rav_requester_single(&self) -> Result<()> {
        let RAVRequest {
            valid_receipts,
            previous_rav,
            invalid_receipts,
            expected_rav,
        } = self
            .tap_manager
            .create_rav_request(
                self.config.tap.rav_request_timestamp_buffer_ms * 1_000_000,
                // TODO: limit the number of receipts to aggregate per request.
                None,
            )
            .await
            .map_err(|e| match e {
                tap_core::Error::NoValidReceiptsForRAVRequest => anyhow!(
                    "It looks like there are no valid receipts for the RAV request.\
                 This may happen if your `rav_request_trigger_value` is too low \
                 and no receipts were found outside the `rav_request_timestamp_buffer_ms`.\
                 You can fix this by increasing the `rav_request_trigger_value`."
                ),
                _ => e.into(),
            })?;
        if !invalid_receipts.is_empty() {
            warn!(
                "Found {} invalid receipts for allocation {} and sender {}.",
                invalid_receipts.len(),
                self.allocation_id,
                self.sender
            );

            // Save invalid receipts to the database for logs.
            // TODO: consider doing that in a spawned task?
            Self::store_invalid_receipts(self, invalid_receipts.as_slice()).await?;
        }
        let client = HttpClientBuilder::default()
            .request_timeout(Duration::from_secs(
                self.config.tap.rav_request_timeout_secs,
            ))
            .build(&self.sender_aggregator_endpoint)?;
        let response: JsonRpcResponse<EIP712SignedMessage<ReceiptAggregateVoucher>> = client
            .request(
                "aggregate_receipts",
                rpc_params!(
                    "0.0", // TODO: Set the version in a smarter place.
                    valid_receipts,
                    previous_rav
                ),
            )
            .await?;
        if let Some(warnings) = response.warnings {
            warn!("Warnings from sender's TAP aggregator: {:?}", warnings);
        }
        match self
            .tap_manager
            .verify_and_store_rav(expected_rav.clone(), response.data.clone())
            .await
        {
            Ok(_) => {}

            // Adapter errors are local software errors. Shouldn't be a problem with the sender.
            Err(tap_core::Error::AdapterError { source_error: e }) => {
                anyhow::bail!("TAP Adapter error while storing RAV: {:?}", e)
            }

            // The 3 errors below signal an invalid RAV, which should be about problems with the
            // sender. The sender could be malicious.
            Err(
                e @ tap_core::Error::InvalidReceivedRAV {
                    expected_rav: _,
                    received_rav: _,
                }
                | e @ tap_core::Error::SignatureError(_)
                | e @ tap_core::Error::InvalidRecoveredSigner { address: _ },
            ) => {
                Self::store_failed_rav(self, &expected_rav, &response.data, &e.to_string()).await?;
                anyhow::bail!("Invalid RAV, sender could be malicious: {:?}.", e);
            }

            // All relevant errors should be handled above. If we get here, we forgot to handle
            // an error case.
            Err(e) => {
                anyhow::bail!("Error while verifying and storing RAV: {:?}", e);
            }
        }
        Ok(())
    }

    pub async fn mark_rav_last(&self) -> Result<()> {
        let updated_rows = sqlx::query!(
            r#"
                        UPDATE scalar_tap_ravs
                        SET last = true
                        WHERE allocation_id = $1 AND sender_address = $2
                    "#,
            self.allocation_id.encode_hex::<String>(),
            self.sender.encode_hex::<String>(),
        )
        .execute(&self.pgpool)
        .await?;
        if updated_rows.rows_affected() != 1 {
            anyhow::bail!(
                "Expected exactly one row to be updated in the latest RAVs table, \
                        but {} were updated.",
                updated_rows.rows_affected()
            );
        };
        Ok(())
    }

    async fn store_invalid_receipts(&self, receipts: &[ReceiptWithState<Failed>]) -> Result<()> {
        for received_receipt in receipts.iter() {
            let receipt = received_receipt.signed_receipt();
            let allocation_id = receipt.message.allocation_id;
            let encoded_signature = receipt.signature.to_vec();

            let receipt_signer = receipt
                .recover_signer(&self.domain_separator)
                .map_err(|e| {
                    error!("Failed to recover receipt signer: {}", e);
                    anyhow!(e)
                })?;

            sqlx::query!(
                r#"
                    INSERT INTO scalar_tap_receipts_invalid (
                        signer_address,
                        signature,
                        allocation_id,
                        timestamp_ns,
                        nonce,
                        value
                    )
                    VALUES ($1, $2, $3, $4, $5, $6)
                "#,
                receipt_signer.encode_hex::<String>(),
                encoded_signature,
                allocation_id.encode_hex::<String>(),
                BigDecimal::from(receipt.message.timestamp_ns),
                BigDecimal::from(receipt.message.nonce),
                BigDecimal::from(BigInt::from(receipt.message.value)),
            )
            .execute(&self.pgpool)
            .await
            .map_err(|e| anyhow!("Failed to store failed receipt: {:?}", e))?;
        }

        Ok(())
    }

    async fn store_failed_rav(
        &self,
        expected_rav: &ReceiptAggregateVoucher,
        rav: &EIP712SignedMessage<ReceiptAggregateVoucher>,
        reason: &str,
    ) -> Result<()> {
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_rav_requests_failed (
                    allocation_id,
                    sender_address,
                    expected_rav,
                    rav_response,
                    reason
                )
                VALUES ($1, $2, $3, $4, $5)
            "#,
            self.allocation_id.encode_hex::<String>(),
            self.sender.encode_hex::<String>(),
            serde_json::to_value(expected_rav)?,
            serde_json::to_value(rav)?,
            reason
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| anyhow!("Failed to store failed RAV: {:?}", e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_create_sender_allocation() {
        // create an allocation

        // expect nothing fails?
    }

    #[test]
    fn test_receive_new_receipt() {
        // should validate with id less than last_id

        // should emit update aggregate fees message to sender account
    }

    #[test]
    fn test_trigger_rav_request() {
        // should rav request

        // should return correct unaggregated fees
    }

    #[test]
    fn test_close_allocation() {
        // create allocation

        // populate db

        // should trigger rav request

        // should mark rav final

        // should stop actor

        // check if message is sent to sender account
    }

    #[test]
    fn test_store_failed_rav() {
        // check database if failed rav is stored
    }

    #[test]
    fn test_calculate_unaggregated_fee() {
        // add a bunch of receipts

        // calculate unaggregated fee

        // check if it's correct
    }

    #[test]
    fn test_store_invalid_receipts() {
        // verify if invalid receipts are stored correctly
    }

    #[test]
    fn test_mark_rav_final() {
        // mark rav as final

        // check if database contains final rav
    }
}
