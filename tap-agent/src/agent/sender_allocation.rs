// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::primitives::Address;
use alloy::{dyn_abi::Eip712Domain, hex::ToHexExt};
use anyhow::{anyhow, ensure, Result};
use bigdecimal::num_bigint::BigInt;
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use jsonrpsee::{core::client::ClientT, rpc_params};
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::{
    manager::adapters::RAVRead,
    rav::{RAVRequest, ReceiptAggregateVoucher, SignedRAV},
    receipt::{
        checks::{Check, CheckList},
        state::Failed,
        ReceiptWithState,
    },
    signed_message::EIP712SignedMessage,
};
use tracing::{debug, error, warn};

use crate::{agent::sender_account::ReceiptFees, lazy_static};

use crate::agent::sender_account::SenderAccountMessage;
use crate::agent::sender_accounts_manager::NewReceiptNotification;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::context::{checks::Signature, TapAgentContext},
    tap::signers_trimmed,
    tap::{context::checks::AllocationId, escrow_adapter::EscrowAdapter},
};
use thiserror::Error;

lazy_static! {
    static ref CLOSED_SENDER_ALLOCATIONS: CounterVec = register_counter_vec!(
        "tap_closed_sender_allocation_total",
        "Count of sender-allocation managers closed since the start of the program",
        &["sender"]
    )
    .unwrap();
    static ref RAVS_CREATED: CounterVec = register_counter_vec!(
        "tap_ravs_created_total",
        "RAVs updated or created per sender allocation since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
    static ref RAVS_FAILED: CounterVec = register_counter_vec!(
        "tap_ravs_failed_total",
        "RAV requests failed since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
    static ref RAV_RESPONSE_TIME: HistogramVec = register_histogram_vec!(
        "tap_rav_response_time_seconds",
        "RAV response time per sender",
        &["sender"]
    )
    .unwrap();
}

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

pub struct SenderAllocationState {
    unaggregated_fees: UnaggregatedReceipts,
    invalid_receipts_fees: UnaggregatedReceipts,
    latest_rav: Option<SignedRAV>,
    pgpool: PgPool,
    tap_manager: TapManager,
    allocation_id: Address,
    sender: Address,
    config: &'static config::Config,
    escrow_accounts: Eventual<EscrowAccounts>,
    domain_separator: Eip712Domain,
    sender_account_ref: ActorRef<SenderAccountMessage>,

    sender_aggregator: jsonrpsee::http_client::HttpClient,
}

pub struct SenderAllocationArgs {
    pub config: &'static config::Config,
    pub pgpool: PgPool,
    pub allocation_id: Address,
    pub sender: Address,
    pub escrow_accounts: Eventual<EscrowAccounts>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub escrow_adapter: EscrowAdapter,
    pub domain_separator: Eip712Domain,
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
    pub sender_aggregator: jsonrpsee::http_client::HttpClient,
}

#[derive(Debug)]
pub enum SenderAllocationMessage {
    NewReceipt(NewReceiptNotification),
    TriggerRAVRequest,
    #[cfg(test)]
    GetUnaggregatedReceipts(ractor::RpcReplyPort<UnaggregatedReceipts>),
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
                state.invalid_receipts_fees.clone(),
            ))?;
        }

        // update unaggregated_fees
        state.unaggregated_fees = state.calculate_unaggregated_fee().await?;

        sender_account_ref.cast(SenderAccountMessage::UpdateReceiptFees(
            allocation_id,
            ReceiptFees::UpdateValue(state.unaggregated_fees.clone()),
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
        // Request a RAV and mark the allocation as final.
        while state.unaggregated_fees.value > 0 {
            if let Err(err) = state.request_rav().await {
                error!(error = %err, "There was an error while requesting rav. Retrying in 30 seconds...");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }

        while let Err(err) = state.mark_rav_last().await {
            error!(error = %err, %state.allocation_id, %state.sender,  "Error while marking allocation last. Retrying in 30 seconds...");
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
                    id, value: fees, ..
                } = notification;
                if id > unaggregated_fees.last_id {
                    unaggregated_fees.last_id = id;
                    unaggregated_fees.value = unaggregated_fees
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
                    // it's fine to crash the actor, could not send a message to its parent
                    state
                        .sender_account_ref
                        .cast(SenderAccountMessage::UpdateReceiptFees(
                            state.allocation_id,
                            ReceiptFees::NewReceipt(fees),
                        ))?;
                }
            }
            SenderAllocationMessage::TriggerRAVRequest => {
                let rav_result = if state.unaggregated_fees.value > 0 {
                    state
                        .request_rav()
                        .await
                        .map(|_| (state.unaggregated_fees.clone(), state.latest_rav.clone()))
                } else {
                    Err(anyhow!("Unaggregated fee equals zero"))
                };

                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        state.allocation_id,
                        ReceiptFees::RavRequestResponse(rav_result),
                    ))?;
            }
            #[cfg(test)]
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(unaggregated_fees.clone());
                }
            }
        }

        Ok(())
    }
}

impl SenderAllocationState {
    async fn new(
        SenderAllocationArgs {
            config,
            pgpool,
            allocation_id,
            sender,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            domain_separator,
            sender_account_ref,
            sender_aggregator,
        }: SenderAllocationArgs,
    ) -> anyhow::Result<Self> {
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
            config,
            escrow_accounts,
            domain_separator,
            sender_account_ref: sender_account_ref.clone(),
            unaggregated_fees: UnaggregatedReceipts::default(),
            invalid_receipts_fees: UnaggregatedReceipts::default(),
            latest_rav,
            sender_aggregator,
        })
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_unaggregated_fee(&self) -> Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_unaggregated_fee()");
        self.tap_manager.remove_obsolete_receipts().await?;

        let signers = signers_trimmed(&self.escrow_accounts, self.sender).await?;

        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value)
            FROM
                scalar_tap_receipts
            WHERE
                allocation_id = $1
                AND signer_address IN (SELECT unnest($2::text[]))
                AND timestamp_ns > $3
            "#,
            self.allocation_id.encode_hex(),
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
        })
    }

    async fn calculate_invalid_receipts_fee(&self) -> Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_invalid_receipts_fee()");
        let signers = signers_trimmed(&self.escrow_accounts, self.sender).await?;

        // TODO: Get `rav.timestamp_ns` from the TAP Manager's RAV storage adapter instead?
        let res = sqlx::query!(
            r#"
            SELECT
                MAX(id),
                SUM(value)
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
        })
    }

    async fn request_rav(&mut self) -> Result<()> {
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
                self.config.tap.rav_request_timestamp_buffer_ms * 1_000_000,
                Some(self.config.tap.rav_request_receipt_limit),
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
                let signers = signers_trimmed(&self.escrow_accounts, self.sender).await?;
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
            (Err(_), true, true) => Err(anyhow!(
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

    async fn store_invalid_receipts(
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
                self.invalid_receipts_fees.clone(),
            ))?;

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

#[cfg(test)]
pub mod tests {
    use super::{
        SenderAllocation, SenderAllocationArgs, SenderAllocationMessage, SenderAllocationState,
    };
    use crate::{
        agent::{
            sender_account::{ReceiptFees, SenderAccountMessage},
            sender_accounts_manager::NewReceiptNotification,
            unaggregated_receipts::UnaggregatedReceipts,
        },
        config,
        tap::{
            escrow_adapter::EscrowAdapter,
            test_utils::{
                create_rav, create_received_receipt, store_invalid_receipt, store_rav,
                store_receipt, ALLOCATION_ID_0, INDEXER, SENDER, SIGNER,
                TAP_EIP712_DOMAIN_SEPARATOR,
            },
        },
    };
    use eventuals::Eventual;
    use futures::future::join_all;
    use indexer_common::{
        escrow_accounts::EscrowAccounts,
        subgraph_client::{DeploymentDetails, SubgraphClient},
    };
    use jsonrpsee::http_client::HttpClientBuilder;
    use ractor::{
        call, cast, concurrency::JoinHandle, Actor, ActorProcessingErr, ActorRef, ActorStatus,
    };
    use ruint::aliases::U256;
    use serde_json::json;
    use sqlx::PgPool;
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tap_aggregator::{jsonrpsee_helpers::JsonRpcResponse, server::run_server};
    use tap_core::receipt::{
        checks::{Check, CheckError, CheckList, CheckResult},
        state::Checking,
        ReceiptWithState,
    };
    use tokio::sync::mpsc;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, Respond, ResponseTemplate,
    };

    const DUMMY_URL: &str = "http://localhost:1234";

    pub struct MockSenderAccount {
        pub last_message_emitted: tokio::sync::mpsc::Sender<SenderAccountMessage>,
    }

    #[async_trait::async_trait]
    impl Actor for MockSenderAccount {
        type Msg = SenderAccountMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _allocation_ids: Self::Arguments,
        ) -> std::result::Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> std::result::Result<(), ActorProcessingErr> {
            self.last_message_emitted.send(message).await.unwrap();
            Ok(())
        }
    }

    async fn create_mock_sender_account() -> (
        mpsc::Receiver<SenderAccountMessage>,
        ActorRef<SenderAccountMessage>,
        JoinHandle<()>,
    ) {
        let (last_message_emitted, rx) = mpsc::channel(64);

        let (sender_account, join_handle) = MockSenderAccount::spawn(
            None,
            MockSenderAccount {
                last_message_emitted: last_message_emitted.clone(),
            },
            (),
        )
        .await
        .unwrap();
        (rx, sender_account, join_handle)
    }

    async fn create_sender_allocation_args(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> SenderAllocationArgs {
        let config = Box::leak(Box::new(config::Config {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: 100,
                rav_request_timestamp_buffer_ms: 1,
                rav_request_timeout_secs: 5,
                rav_request_receipt_limit: 1000,
                ..Default::default()
            },
            ..Default::default()
        }));

        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
        )));

        let escrow_accounts_eventual = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone(), SENDER.1);

        let sender_account_ref = match sender_account {
            Some(sender) => sender,
            None => create_mock_sender_account().await.1,
        };

        let sender_aggregator = HttpClientBuilder::default()
            .build(&sender_aggregator_endpoint)
            .unwrap();
        SenderAllocationArgs {
            config,
            pgpool: pgpool.clone(),
            allocation_id: *ALLOCATION_ID_0,
            sender: SENDER.1,
            escrow_accounts: escrow_accounts_eventual,
            escrow_subgraph,
            escrow_adapter,
            domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_account_ref,
            sender_aggregator,
        }
    }

    async fn create_sender_allocation(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> ActorRef<SenderAllocationMessage> {
        let args = create_sender_allocation_args(
            pgpool,
            sender_aggregator_endpoint,
            escrow_subgraph_endpoint,
            sender_account,
        )
        .await;

        let (allocation_ref, _join_handle) = SenderAllocation::spawn(None, SenderAllocation, args)
            .await
            .unwrap();

        allocation_ref
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_update_unaggregated_fees_on_start(pgpool: PgPool) {
        let (mut last_message_emitted, sender_account, _join_handle) =
            create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            DUMMY_URL.to_string(),
            DUMMY_URL,
            Some(sender_account),
        )
        .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Should emit a message to the sender account with the unaggregated fees.
        let expected_message = SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                last_id: 10,
                value: 55u128,
            }),
        );
        let last_message_emitted = last_message_emitted.recv().await.unwrap();
        assert_eq!(last_message_emitted, expected_message);

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 55u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_return_invalid_receipts_on_startup(pgpool: PgPool) {
        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_invalid_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            DUMMY_URL.to_string(),
            DUMMY_URL,
            Some(sender_account),
        )
        .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Should emit a message to the sender account with the unaggregated fees.
        let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
            *ALLOCATION_ID_0,
            UnaggregatedReceipts {
                last_id: 10,
                value: 55u128,
            },
        );
        let update_invalid_msg = message_receiver.recv().await.unwrap();
        assert_eq!(update_invalid_msg, expected_message);
        let last_message_emitted = message_receiver.recv().await.unwrap();
        assert_eq!(
            last_message_emitted,
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
            )
        );

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_receive_new_receipt(pgpool: PgPool) {
        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;

        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            DUMMY_URL.to_string(),
            DUMMY_URL,
            Some(sender_account),
        )
        .await;

        // should validate with id less than last_id
        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 0,
                value: 10,
                allocation_id: *ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns: 0,
            })
        )
        .unwrap();

        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 1,
                value: 20,
                allocation_id: *ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns: 0,
            })
        )
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // should emit update aggregate fees message to sender account
        let expected_message = SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(20u128),
        );
        let startup_load_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_load_msg,
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 0,
                    last_id: 0
                })
            )
        );
        let last_message_emitted = message_receiver.recv().await.unwrap();
        assert_eq!(last_message_emitted, expected_message);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_trigger_rav_request(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SIGNER.0.clone(),
            vec![SIGNER.1].into_iter().collect(),
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            100 * 1024,
            100 * 1024,
            1,
        )
        .await
        .unwrap();

        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();

            // store a copy that should fail in the uniqueness test
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;

        // Create a sender_allocation.
        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            "http://".to_owned() + &aggregator_endpoint.to_string(),
            &mock_server.uri(),
            Some(sender_account),
        )
        .await;

        // Trigger a RAV request manually and wait for updated fees.
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRAVRequest)
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);

        let startup_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 90,
                    last_id: 20
                })
            )
        );

        // Check if the sender received invalid receipt fees
        let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
            *ALLOCATION_ID_0,
            UnaggregatedReceipts {
                last_id: 0,
                value: 45u128,
            },
        );
        assert_eq!(message_receiver.recv().await.unwrap(), expected_message);

        assert!(matches!(
            message_receiver.recv().await.unwrap(),
            SenderAccountMessage::UpdateReceiptFees(_, ReceiptFees::RavRequestResponse(_))
        ));

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_close_allocation_no_pending_fees(pgpool: PgPool) {
        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;

        // create allocation
        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            DUMMY_URL.to_string(),
            DUMMY_URL,
            Some(sender_account),
        )
        .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        // check if message is sent to sender account
        assert_eq!(
            message_receiver.recv().await.unwrap(),
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
            )
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_close_allocation_with_pending_fees(pgpool: PgPool) {
        struct Response {
            data: Arc<tokio::sync::Notify>,
        }

        impl Respond for Response {
            fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
                self.data.notify_one();

                let mock_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 10, 45);

                let json_response = JsonRpcResponse {
                    data: mock_rav,
                    warnings: None,
                };

                ResponseTemplate::new(200).set_body_json(json! (
                    {
                        "id": 0,
                        "jsonrpc": "2.0",
                        "result": json_response
                    }
                ))
            }
        }

        let await_trigger = Arc::new(tokio::sync::Notify::new());
        // Start a TAP aggregator server.
        let aggregator_server = MockServer::start().await;

        aggregator_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("aggregate_receipts"))
                    .respond_with(Response {
                        data: await_trigger.clone(),
                    }),
            )
            .await;

        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (_last_message_emitted, sender_account, _join_handle) =
            create_mock_sender_account().await;

        // create allocation
        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            aggregator_server.uri(),
            &mock_server.uri(),
            Some(sender_account),
        )
        .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();

        // should trigger rav request
        await_trigger.notified().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // check if rav request is made
        assert!(aggregator_server.received_requests().await.is_some());

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_return_unaggregated_fees_without_rav(pgpool: PgPool) {
        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // calculate unaggregated fee
        let total_unaggregated_fees = state.calculate_unaggregated_fee().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_calculate_invalid_receipts_fee(pgpool: PgPool) {
        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_invalid_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // calculate invalid unaggregated fee
        let total_invalid_receipts = state.calculate_invalid_receipts_fee().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_invalid_receipts.value, 45u128);
    }

    /// Test that the sender_allocation correctly updates the unaggregated fees from the
    /// database when there is a RAV in the database as well as receipts which timestamp are lesser
    /// and greater than the RAV's timestamp.
    ///
    /// The sender_allocation should only consider receipts with a timestamp greater
    /// than the RAV's timestamp.
    #[sqlx::test(migrations = "../migrations")]
    async fn should_return_unaggregated_fees_with_rav(pgpool: PgPool) {
        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add the RAV to the database.
        // This RAV has timestamp 4. The sender_allocation should only consider receipts
        // with a timestamp greater than 4.
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let total_unaggregated_fees = state.calculate_unaggregated_fee().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 35u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_store_failed_rav(pgpool: PgPool) {
        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);

        // just unit test if it is working
        let result = state
            .store_failed_rav(&signed_rav.message, &signed_rav, "test")
            .await;

        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_store_invalid_receipts(pgpool: PgPool) {
        struct FailingCheck;

        #[async_trait::async_trait]
        impl Check for FailingCheck {
            async fn check(&self, _receipt: &ReceiptWithState<Checking>) -> CheckResult {
                Err(CheckError::Failed(anyhow::anyhow!("Failing check")))
            }
        }

        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let mut state = SenderAllocationState::new(args).await.unwrap();

        let checks = CheckList::new(vec![Arc::new(FailingCheck)]);

        // create some checks
        let checking_receipts = vec![
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
        ];
        // make sure to fail them
        let failing_receipts = checking_receipts
            .into_iter()
            .map(|receipt| async {
                receipt
                    .finalize_receipt_checks(&checks)
                    .await
                    .unwrap()
                    .unwrap_err()
            })
            .collect::<Vec<_>>();
        let failing_receipts: Vec<_> = join_all(failing_receipts).await;

        // store the failing receipts
        let result = state.store_invalid_receipts(&failing_receipts).await;

        // we just store a few and make sure it doesn't fail
        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_mark_rav_last(pgpool: PgPool) {
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // mark rav as final
        let result = state.mark_rav_last().await;

        // check if it fails
        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_failed_rav_request(pgpool: PgPool) {
        // Add receipts to the database.
        for i in 0..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, u64::MAX, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;

        // Create a sender_allocation.
        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            DUMMY_URL.to_string(),
            DUMMY_URL,
            Some(sender_account),
        )
        .await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRAVRequest)
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 45,
                    last_id: 10
                })
            )
        );
        let rav_response_message = message_receiver.recv().await.unwrap();
        match rav_response_message {
            SenderAccountMessage::UpdateReceiptFees(
                _,
                ReceiptFees::RavRequestResponse(rav_response),
            ) => {
                assert!(rav_response.is_err());
            }
            v => panic!("Expecting RavRequestResponse as last message, found: {v:?}"),
        }

        // expect the actor to keep running
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

        // Check that the unaggregated fees return the same value
        // TODO: Maybe this can no longer be checked?
        //assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_request_when_all_receipts_invalid(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (_handle, aggregator_endpoint) = run_server(
            0,
            SIGNER.0.clone(),
            vec![SIGNER.1].into_iter().collect(),
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            100 * 1024,
            100 * 1024,
            1,
        )
        .await
        .unwrap();

        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;
        // Add invalid receipts to the database. ( receipts are older than rav )
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos() as u64
            - 10000;
        const RECEIPT_VALUE: u128 = 1622018441284756158;
        const TOTAL_RECEIPTS: u64 = 10;
        const TOTAL_SUM: u128 = RECEIPT_VALUE * TOTAL_RECEIPTS as u128;

        for i in 0..TOTAL_RECEIPTS {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, timestamp, RECEIPT_VALUE);
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account, _join_handle) =
            create_mock_sender_account().await;

        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            "http://".to_owned() + &aggregator_endpoint.to_string(),
            &mock_server.uri(),
            Some(sender_account),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRAVRequest)
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 16220184412847561580,
                    last_id: 10
                })
            )
        );

        let invalid_receipts = message_receiver.recv().await.unwrap();

        assert_eq!(
            invalid_receipts,
            SenderAccountMessage::UpdateInvalidReceiptFees(
                *ALLOCATION_ID_0,
                UnaggregatedReceipts {
                    value: TOTAL_SUM,
                    last_id: 0
                }
            )
        );

        let rav_response_message = message_receiver.recv().await.unwrap();
        match rav_response_message {
            SenderAccountMessage::UpdateReceiptFees(
                _,
                ReceiptFees::RavRequestResponse(rav_response),
            ) => {
                assert!(rav_response.is_err());
            }
            v => panic!("Expecting RavRequestResponse as last message, found: {v:?}"),
        }

        let invalid_receipts = sqlx::query!(
            r#"
                SELECT * FROM scalar_tap_receipts_invalid;
            "#,
        )
        .fetch_all(&pgpool)
        .await
        .expect("Should not fail to fetch from scalar_tap_receipts_invalid");

        // Invalid receipts should be found inside the table
        assert_eq!(invalid_receipts.len(), 10);

        // make sure scalar_tap_receipts gets emptied
        let all_receipts = sqlx::query!(
            r#"
                SELECT * FROM scalar_tap_receipts;
            "#,
        )
        .fetch_all(&pgpool)
        .await
        .expect("Should not fail to fetch from scalar_tap_receipts");

        // Invalid receipts should be found inside the table
        assert!(all_receipts.is_empty());
    }
}
