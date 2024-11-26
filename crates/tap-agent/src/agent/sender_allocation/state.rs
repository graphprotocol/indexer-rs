// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Instant};

use alloy::primitives::Address;
use alloy::{dyn_abi::Eip712Domain, hex::ToHexExt};
use anyhow::{anyhow, ensure, Result};
use bigdecimal::{num_bigint::BigInt, ToPrimitive};
use indexer_monitor::EscrowAccounts;
use jsonrpsee::{core::client::ClientT, rpc_params};
use ractor::ActorRef;
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::{
    manager::adapters::RAVRead,
    rav::{RAVRequest, ReceiptAggregateVoucher, SignedRAV},
    receipt::{
        checks::{Check, CheckList},
        state::Failed,
        Context, ReceiptWithState,
    },
    signed_message::EIP712SignedMessage,
};
use tokio::sync::watch::Receiver;
use tracing::{debug, error, warn};

use crate::agent::sender_account::SenderAccountMessage;
use crate::agent::sender_allocation::RAV_RESPONSE_TIME;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    tap::context::checks::AllocationId,
    tap::context::{checks::Signature, TapAgentContext},
    tap::signers_trimmed,
};

use super::{RavError, SenderAllocationArgs, TapManager, RAVS_CREATED, RAVS_FAILED};

pub struct SenderAllocationState {
    pub(super) unaggregated_fees: UnaggregatedReceipts,
    pub(super) invalid_receipts_fees: UnaggregatedReceipts,
    pub(super) latest_rav: Option<SignedRAV>,
    pub(super) pgpool: PgPool,
    pub(super) tap_manager: TapManager,
    pub(super) allocation_id: Address,
    pub(super) sender: Address,
    pub(super) escrow_accounts: Receiver<EscrowAccounts>,
    pub(super) domain_separator: Eip712Domain,
    pub(super) sender_account_ref: ActorRef<SenderAccountMessage>,

    pub(super) sender_aggregator: jsonrpsee::http_client::HttpClient,

    //config
    pub(super) timestamp_buffer_ns: u64,
    pub(super) rav_request_receipt_limit: u64,
}

impl SenderAllocationState {
    pub async fn new(
        SenderAllocationArgs {
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            escrow_subgraph,
            domain_separator,
            sender_account_ref,
            sender_aggregator,
            config,
        }: SenderAllocationArgs,
    ) -> anyhow::Result<Self> {
        let required_checks: Vec<Arc<dyn Check + Send + Sync>> = vec![
            Arc::new(
                AllocationId::new(
                    config.indexer_address,
                    config.escrow_polling_interval,
                    sender,
                    allocation_id,
                    escrow_subgraph,
                )
                .await,
            ),
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
        );
        let latest_rav = context.last_rav().await.unwrap_or_default();
        let tap_manager = TapManager::new(
            domain_separator.clone(),
            context,
            CheckList::new(required_checks),
        );

        Ok(Self {
            pgpool,
            tap_manager,
            allocation_id,
            sender,
            escrow_accounts,
            domain_separator,

            sender_account_ref: sender_account_ref.clone(),
            unaggregated_fees: UnaggregatedReceipts::default(),
            invalid_receipts_fees: UnaggregatedReceipts::default(),
            latest_rav,
            sender_aggregator,
            rav_request_receipt_limit: config.rav_request_receipt_limit,
            timestamp_buffer_ns: config.timestamp_buffer_ns,
        })
    }

    pub async fn recalculate_all_unaggregated_fees(&self) -> Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(i64::MAX).await
    }

    async fn calculate_unaggregated_fee(&self) -> Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(self.unaggregated_fees.last_id as i64)
            .await
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_fee_until_last_id(&self, last_id: i64) -> Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_unaggregated_fee()");
        self.tap_manager.remove_obsolete_receipts().await?;

        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;

        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                scalar_tap_receipts
            WHERE
                allocation_id = $1
                AND id <= $2
                AND signer_address IN (SELECT unnest($3::text[]))
                AND timestamp_ns > $4
            "#,
            self.allocation_id.encode_hex(),
            last_id,
            &signers,
            BigDecimal::from(
                self.latest_rav
                    .as_ref()
                    .map(|rav| rav.message.timestampNs)
                    .unwrap_or_default()
            ),
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
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    pub async fn calculate_invalid_receipts_fee(&self) -> Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_invalid_receipts_fee()");
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;

        // TODO: Get `rav.timestamp_ns` from the TAP Manager's RAV storage adapter instead?
        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value),
                COUNT(*)
            FROM
                scalar_tap_receipts_invalid
            WHERE
                allocation_id = $1
                AND signer_address IN (SELECT unnest($2::text[]))
            "#,
            self.allocation_id.encode_hex(),
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
            counter: res
                .count
                .unwrap_or(0)
                .to_u64()
                .expect("default value exists, this shouldn't be empty"),
        })
    }

    pub async fn request_rav(&mut self) -> Result<()> {
        match self.rav_requester_single().await {
            Ok(rav) => {
                self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                self.latest_rav = Some(rav);
                RAVS_CREATED
                    .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
                    .inc();
                Ok(())
            }
            Err(e) => {
                if let RavError::AllReceiptsInvalid = e {
                    self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                }
                RAVS_FAILED
                    .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
                    .inc();
                Err(e.into())
            }
        }
    }

    /// Request a RAV from the sender's TAP aggregator. Only one RAV request will be running at a
    /// time through the use of an internal guard.
    async fn rav_requester_single(&mut self) -> Result<SignedRAV, RavError> {
        tracing::trace!("rav_requester_single()");
        let RAVRequest {
            valid_receipts,
            previous_rav,
            invalid_receipts,
            expected_rav,
        } = self
            .tap_manager
            .create_rav_request(
                &Context::new(),
                self.timestamp_buffer_ns,
                Some(self.rav_request_receipt_limit),
            )
            .await?;
        match (
            expected_rav,
            valid_receipts.is_empty(),
            invalid_receipts.is_empty(),
        ) {
            // All receipts are invalid
            (Err(tap_core::Error::NoValidReceiptsForRAVRequest), true, false) => {
                warn!(
                    "Found {} invalid receipts for allocation {} and sender {}.",
                    invalid_receipts.len(),
                    self.allocation_id,
                    self.sender
                );
                self.store_invalid_receipts(invalid_receipts.as_slice())
                    .await?;
                // Obtain min/max timestamps to define query
                let min_timestamp = invalid_receipts
                    .iter()
                    .map(|receipt| receipt.signed_receipt().message.timestamp_ns)
                    .min()
                    .expect("invalid receipts should not be empty");
                let max_timestamp = invalid_receipts
                    .iter()
                    .map(|receipt| receipt.signed_receipt().message.timestamp_ns)
                    .max()
                    .expect("invalid receipts should not be empty");
                let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender).await?;
                sqlx::query!(
                    r#"
                        DELETE FROM scalar_tap_receipts
                        WHERE timestamp_ns BETWEEN $1 AND $2
                        AND allocation_id = $3
                        AND signer_address IN (SELECT unnest($4::text[]));
                    "#,
                    BigDecimal::from(min_timestamp),
                    BigDecimal::from(max_timestamp),
                    self.allocation_id.encode_hex(),
                    &signers,
                )
                .execute(&self.pgpool)
                .await?;
                Err(RavError::AllReceiptsInvalid)
            }
            // When it receives both valid and invalid receipts or just valid
            (Ok(expected_rav), ..) => {
                let valid_receipts: Vec<_> = valid_receipts
                    .into_iter()
                    .map(|r| r.signed_receipt().clone())
                    .collect();
                let rav_response_time_start = Instant::now();
                let response: JsonRpcResponse<EIP712SignedMessage<ReceiptAggregateVoucher>> = self
                    .sender_aggregator
                    .request(
                        "aggregate_receipts",
                        rpc_params!(
                            "0.0", // TODO: Set the version in a smarter place.
                            valid_receipts,
                            previous_rav
                        ),
                    )
                    .await
                    .inspect_err(|err| {
                        if let jsonrpsee::core::ClientError::RequestTimeout = &err {
                            warn!(
                                "Rav request is timing out, maybe request_timeout_secs is too \
                                low in your config file, try adding more secs to the value. \
                                If the problem persists after doing so please open an issue"
                            );
                        }
                    })?;

                let rav_response_time = rav_response_time_start.elapsed();
                RAV_RESPONSE_TIME
                    .with_label_values(&[&self.sender.to_string()])
                    .observe(rav_response_time.as_secs_f64());
                // we only save invalid receipts when we are about to store our rav
                //
                // store them before we call remove_obsolete_receipts()
                if !invalid_receipts.is_empty() {
                    warn!(
                        "Found {} invalid receipts for allocation {} and sender {}.",
                        invalid_receipts.len(),
                        self.allocation_id,
                        self.sender
                    );

                    // Save invalid receipts to the database for logs.
                    // TODO: consider doing that in a spawned task?
                    self.store_invalid_receipts(invalid_receipts.as_slice())
                        .await?;
                }

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
                        return Err(
                            anyhow::anyhow!("TAP Adapter error while storing RAV: {:?}", e).into(),
                        )
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
                        Self::store_failed_rav(self, &expected_rav, &response.data, &e.to_string())
                            .await?;
                        return Err(anyhow::anyhow!(
                            "Invalid RAV, sender could be malicious: {:?}.",
                            e
                        )
                        .into());
                    }

                    // All relevant errors should be handled above. If we get here, we forgot to handle
                    // an error case.
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error while verifying and storing RAV: {:?}",
                            e
                        )
                        .into());
                    }
                }
                Ok(response.data)
            }
            (Err(tap_core::Error::NoValidReceiptsForRAVRequest), true, true) => Err(anyhow!(
                "It looks like there are no valid receipts for the RAV request.\
                This may happen if your `rav_request_trigger_value` is too low \
                and no receipts were found outside the `rav_request_timestamp_buffer_ms`.\
                You can fix this by increasing the `rav_request_trigger_value`."
            )
            .into()),
            (Err(e), ..) => Err(e.into()),
        }
    }

    pub async fn mark_rav_last(&self) -> Result<()> {
        tracing::info!(
            sender = %self.sender,
            allocation_id = %self.allocation_id,
            "Marking rav as last!",
        );
        let updated_rows = sqlx::query!(
            r#"
                        UPDATE scalar_tap_ravs
                        SET last = true
                        WHERE allocation_id = $1 AND sender_address = $2
                    "#,
            self.allocation_id.encode_hex(),
            self.sender.encode_hex(),
        )
        .execute(&self.pgpool)
        .await?;

        match updated_rows.rows_affected() {
            // in case no rav was marked as final
            0 => {
                warn!(
                    "No RAVs were updated as last for allocation {} and sender {}.",
                    self.allocation_id, self.sender
                );
                Ok(())
            }
            1 => Ok(()),
            _ => anyhow::bail!(
                "Expected exactly one row to be updated in the latest RAVs table, \
                        but {} were updated.",
                updated_rows.rows_affected()
            ),
        }
    }

    pub async fn store_invalid_receipts(
        &mut self,
        receipts: &[ReceiptWithState<Failed>],
    ) -> Result<()> {
        let reciepts_len = receipts.len();
        let mut reciepts_signers = Vec::with_capacity(reciepts_len);
        let mut encoded_signatures = Vec::with_capacity(reciepts_len);
        let mut allocation_ids = Vec::with_capacity(reciepts_len);
        let mut timestamps = Vec::with_capacity(reciepts_len);
        let mut nounces = Vec::with_capacity(reciepts_len);
        let mut values = Vec::with_capacity(reciepts_len);
        let mut error_logs = Vec::with_capacity(reciepts_len);

        for received_receipt in receipts.iter() {
            let receipt = received_receipt.signed_receipt();
            let allocation_id = receipt.message.allocation_id;
            let encoded_signature = receipt.signature.as_bytes().to_vec();
            let receipt_error = received_receipt.clone().error().to_string();
            let receipt_signer = receipt
                .recover_signer(&self.domain_separator)
                .map_err(|e| {
                    error!("Failed to recover receipt signer: {}", e);
                    anyhow!(e)
                })?;
            debug!(
                "Receipt for allocation {} and signer {} failed reason: {}",
                allocation_id.encode_hex(),
                receipt_signer.encode_hex(),
                receipt_error
            );
            reciepts_signers.push(receipt_signer.encode_hex());
            encoded_signatures.push(encoded_signature);
            allocation_ids.push(allocation_id.encode_hex());
            timestamps.push(BigDecimal::from(receipt.message.timestamp_ns));
            nounces.push(BigDecimal::from(receipt.message.nonce));
            values.push(BigDecimal::from(BigInt::from(receipt.message.value)));
            error_logs.push(receipt_error);
        }
        sqlx::query!(
            r#"INSERT INTO scalar_tap_receipts_invalid (
                signer_address,
                signature,
                allocation_id,
                timestamp_ns,
                nonce,
                value,
                error_log
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(40)[],
                $4::NUMERIC(20)[],
                $5::NUMERIC(20)[],
                $6::NUMERIC(40)[],
                $7::TEXT[]
            )"#,
            &reciepts_signers,
            &encoded_signatures,
            &allocation_ids,
            &timestamps,
            &nounces,
            &values,
            &error_logs
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            error!("Failed to store invalid receipt: {}", e);
            anyhow!(e)
        })?;

        let fees = receipts
            .iter()
            .map(|receipt| receipt.signed_receipt().message.value)
            .sum();

        self.invalid_receipts_fees.value = self
            .invalid_receipts_fees
            .value
            .checked_add(fees)
            .unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                error!(
                    "Overflow when adding receipt value {} to invalid receipts fees {} \
            for allocation {} and sender {}. Setting total unaggregated fees to \
            u128::MAX.",
                    fees, self.invalid_receipts_fees.value, self.allocation_id, self.sender
                );
                u128::MAX
            });
        self.sender_account_ref
            .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                self.allocation_id,
                self.invalid_receipts_fees,
            ))?;

        Ok(())
    }

    pub async fn store_failed_rav(
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
            self.allocation_id.encode_hex(),
            self.sender.encode_hex(),
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
