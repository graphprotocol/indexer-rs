// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure};
use bigdecimal::{num_bigint::BigInt, ToPrimitive};
use indexer_monitor::{EscrowAccounts, SubgraphClient};
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::grpc::{
    tap_aggregator_client::TapAggregatorClient, RavRequest as AggregatorRequest,
};
use tap_core::{
    manager::adapters::RavRead,
    rav_request::RavRequest,
    receipt::{
        checks::{Check, CheckList},
        rav::AggregationError,
        state::Failed,
        Context, ReceiptWithState, WithValueAndTimestamp,
    },
    signed_message::Eip712SignedMessage,
};
use tap_graph::{ReceiptAggregateVoucher, SignedRav};
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address, sol_types::Eip712Domain};
use thiserror::Error;
use tokio::sync::watch::Receiver;
use tonic::{transport::Channel, Code, Status};

use super::sender_account::SenderAccountConfig;
use crate::{
    agent::{
        sender_account::{ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::NewReceiptNotification,
        unaggregated_receipts::UnaggregatedReceipts,
    },
    lazy_static,
    tap::{
        context::{
            checks::{AllocationId, Signature},
            TapAgentContext,
        },
        signers_trimmed, TapReceipt,
    },
};

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
    AggregationError(#[from] AggregationError),

    #[error(transparent)]
    Grpc(#[from] tonic::Status),

    #[error("All receipts are invalid")]
    AllReceiptsInvalid,

    #[error("Receipt is not legacy")]
    ReceiptNotCompatible,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type TapManager = tap_core::manager::Manager<TapAgentContext, TapReceipt>;

pub enum AllocationType {
    Legacy,
    Horizon,
}

/// Manages unaggregated fees and the TAP lifecyle for a specific (allocation, sender) pair.
pub struct SenderAllocation;

pub struct SenderAllocationState {
    unaggregated_fees: UnaggregatedReceipts,
    invalid_receipts_fees: UnaggregatedReceipts,
    latest_rav: Option<SignedRav>,
    pgpool: PgPool,
    tap_manager: TapManager,
    allocation_id: Address,
    sender: Address,
    escrow_accounts: Receiver<EscrowAccounts>,
    domain_separator: Eip712Domain,
    sender_account_ref: ActorRef<SenderAccountMessage>,
    sender_aggregator: TapAggregatorClient<Channel>,
    allocation_type: AllocationType,
    //config
    timestamp_buffer_ns: u64,
    rav_request_receipt_limit: u64,
}

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

#[derive(bon::Builder)]
pub struct SenderAllocationArgs {
    pub pgpool: PgPool,
    pub allocation_id: Address,
    pub sender: Address,
    pub escrow_accounts: Receiver<EscrowAccounts>,
    pub escrow_subgraph: &'static SubgraphClient,
    pub domain_separator: Eip712Domain,
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
    pub sender_aggregator: TapAggregatorClient<Channel>,
    #[builder(default = AllocationType::Legacy)]
    pub allocation_type: AllocationType,

    //config
    pub config: AllocationConfig,
}

#[derive(Debug)]
pub enum SenderAllocationMessage {
    NewReceipt(NewReceiptNotification),
    TriggerRavRequest,
    #[cfg(any(test, feature = "test"))]
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
    ) -> Result<Self::State, ActorProcessingErr> {
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
    ) -> Result<(), ActorProcessingErr> {
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
                    tracing::error!(
                        error = %err,
                        "There was an error while calculating the last unaggregated receipts. Retrying in 30 seconds...");
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        }
        // Request a RAV and mark the allocation as final.
        while state.unaggregated_fees.value > 0 {
            if let Err(err) = state.request_rav().await {
                tracing::error!(error = %err, "There was an error while requesting rav. Retrying in 30 seconds...");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }

        while let Err(err) = state.mark_rav_last().await {
            tracing::error!(
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
    ) -> Result<(), ActorProcessingErr> {
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
                    tracing::warn!(
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
                            tracing::error!(
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
            SenderAllocationMessage::TriggerRavRequest => {
                let rav_result = if state.unaggregated_fees.value > 0 {
                    state.request_rav().await.map(|_| state.latest_rav.clone())
                } else {
                    Err(anyhow!("Unaggregated fee equals zero"))
                };
                let rav_response = (state.unaggregated_fees, rav_result);
                state
                    .sender_account_ref
                    .cast(SenderAccountMessage::UpdateReceiptFees(
                        state.allocation_id,
                        ReceiptFees::RavRequestResponse(rav_response),
                    ))?;
            }
            #[cfg(any(test, feature = "test"))]
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(*unaggregated_fees);
                }
            }
        }

        Ok(())
    }
}

impl SenderAllocationState {
    async fn new(
        SenderAllocationArgs {
            allocation_type,
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
        let required_checks: Vec<Arc<dyn Check<TapReceipt> + Send + Sync>> = vec![
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
            allocation_type,
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

    async fn recalculate_all_unaggregated_fees(&self) -> anyhow::Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(i64::MAX).await
    }

    async fn calculate_unaggregated_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
        self.calculate_fee_until_last_id(self.unaggregated_fees.last_id as i64)
            .await
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_fee_until_last_id(
        &self,
        last_id: i64,
    ) -> anyhow::Result<UnaggregatedReceipts> {
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

    async fn calculate_invalid_receipts_fee(&self) -> anyhow::Result<UnaggregatedReceipts> {
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

    async fn request_rav(&mut self) -> anyhow::Result<()> {
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
    async fn rav_requester_single(&mut self) -> Result<SignedRav, RavError> {
        tracing::trace!("rav_requester_single()");
        let RavRequest {
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
            (Err(AggregationError::NoValidReceiptsForRavRequest), true, false) => {
                tracing::warn!(
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
                    .map(|receipt| receipt.signed_receipt().timestamp_ns())
                    .min()
                    .expect("invalid receipts should not be empty");
                let max_timestamp = invalid_receipts
                    .iter()
                    .map(|receipt| receipt.signed_receipt().timestamp_ns())
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
                    .map(|r| {
                        r.signed_receipt()
                            .clone()
                            .as_v1()
                            .ok_or(RavError::ReceiptNotCompatible)
                    })
                    .collect::<Result<_, _>>()?;

                let rav_request = AggregatorRequest::new(valid_receipts, previous_rav);

                let rav_response_time_start = Instant::now();

                let response = self
                    .sender_aggregator
                    .aggregate_receipts(rav_request)
                    .await
                    .inspect_err(|status: &Status| {
                        if status.code() == Code::DeadlineExceeded {
                            tracing::warn!(
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
                    tracing::warn!(
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
                let signed_rav = response.into_inner().signed_rav()?;

                match self
                    .tap_manager
                    .verify_and_store_rav(expected_rav.clone(), signed_rav.clone())
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
                        e @ tap_core::Error::InvalidReceivedRav {
                            expected_rav: _,
                            received_rav: _,
                        }
                        | e @ tap_core::Error::SignatureError(_)
                        | e @ tap_core::Error::InvalidRecoveredSigner { address: _ },
                    ) => {
                        Self::store_failed_rav(self, &expected_rav, &signed_rav, &e.to_string())
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
                Ok(signed_rav)
            }
            (Err(AggregationError::NoValidReceiptsForRavRequest), true, true) => Err(anyhow!(
                "It looks like there are no valid receipts for the RAV request.\
                This may happen if your `rav_request_trigger_value` is too low \
                and no receipts were found outside the `rav_request_timestamp_buffer_ms`.\
                You can fix this by increasing the `rav_request_trigger_value`."
            )
            .into()),
            (Err(e), ..) => Err(e.into()),
        }
    }

    pub async fn mark_rav_last(&self) -> anyhow::Result<()> {
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
                tracing::warn!(
                    "No RAVs were updated as last for allocation {} and sender {}.",
                    self.allocation_id,
                    self.sender
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
        receipts: &[ReceiptWithState<Failed, TapReceipt>],
    ) -> anyhow::Result<()> {
        let reciepts_len = receipts.len();
        let mut reciepts_signers = Vec::with_capacity(reciepts_len);
        let mut encoded_signatures = Vec::with_capacity(reciepts_len);
        let mut allocation_ids = Vec::with_capacity(reciepts_len);
        let mut timestamps = Vec::with_capacity(reciepts_len);
        let mut nounces = Vec::with_capacity(reciepts_len);
        let mut values = Vec::with_capacity(reciepts_len);
        let mut error_logs = Vec::with_capacity(reciepts_len);

        for received_receipt in receipts.iter() {
            let receipt = match received_receipt.signed_receipt() {
                TapReceipt::V1(receipt) => receipt,
                TapReceipt::V2(_) => unimplemented!("V2 not supported"),
            };
            let allocation_id = receipt.message.allocation_id;
            let encoded_signature = receipt.signature.as_bytes().to_vec();
            let receipt_error = received_receipt.clone().error().to_string();
            let receipt_signer = receipt
                .recover_signer(&self.domain_separator)
                .map_err(|e| {
                    tracing::error!("Failed to recover receipt signer: {}", e);
                    anyhow!(e)
                })?;
            tracing::debug!(
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
            tracing::error!("Failed to store invalid receipt: {}", e);
            anyhow!(e)
        })?;

        let fees = receipts
            .iter()
            .map(|receipt| receipt.signed_receipt().value())
            .sum();

        self.invalid_receipts_fees.value = self
            .invalid_receipts_fees
            .value
            .checked_add(fees)
            .unwrap_or_else(|| {
                // This should never happen, but if it does, we want to know about it.
                tracing::error!(
                    "Overflow when adding receipt value {} to invalid receipts fees {} \
            for allocation {} and sender {}. Setting total unaggregated fees to \
            u128::MAX.",
                    fees,
                    self.invalid_receipts_fees.value,
                    self.allocation_id,
                    self.sender
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

    async fn store_failed_rav(
        &self,
        expected_rav: &ReceiptAggregateVoucher,
        rav: &Eip712SignedMessage<ReceiptAggregateVoucher>,
        reason: &str,
    ) -> anyhow::Result<()> {
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
    use std::{
        collections::HashMap,
        future::Future,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use bigdecimal::ToPrimitive;
    use futures::future::join_all;
    use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
    use indexer_receipt::TapReceipt;
    use ractor::{call, cast, Actor, ActorRef, ActorStatus};
    use ruint::aliases::U256;
    use serde_json::json;
    use sqlx::PgPool;
    use tap_aggregator::grpc::{tap_aggregator_client::TapAggregatorClient, RavResponse};
    use tap_core::receipt::{
        checks::{Check, CheckError, CheckList, CheckResult},
        Context,
    };
    use test_assets::{
        flush_messages, ALLOCATION_ID_0, TAP_EIP712_DOMAIN as TAP_EIP712_DOMAIN_SEPARATOR,
        TAP_SENDER as SENDER, TAP_SIGNER as SIGNER,
    };
    use tokio::sync::{watch, Notify};
    use tonic::{transport::Endpoint, Code};
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockGuard, MockServer, ResponseTemplate,
    };
    use wiremock_grpc::{MockBuilder, Then};

    use super::{
        SenderAllocation, SenderAllocationArgs, SenderAllocationMessage, SenderAllocationState,
    };
    use crate::{
        agent::{
            sender_account::{ReceiptFees, SenderAccountMessage},
            sender_accounts_manager::NewReceiptNotification,
            unaggregated_receipts::UnaggregatedReceipts,
        },
        tap::CheckingReceipt,
        test::{
            actors::{create_mock_sender_account, TestableActor},
            create_rav, create_received_receipt, get_grpc_url, store_batch_receipts,
            store_invalid_receipt, store_rav, store_receipt, INDEXER,
        },
    };

    async fn mock_escrow_subgraph() -> (MockServer, MockGuard) {
        let mock_ecrow_subgraph_server: MockServer = MockServer::start().await;
        let _mock_ecrow_subgraph = mock_ecrow_subgraph_server
                .register_as_scoped(
                    Mock::given(method("POST"))
                        .and(body_string_contains("TapTransactions"))
                        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                                "transactions": [{
                                    "id": "0x00224ee6ad4ae77b817b4e509dc29d644da9004ad0c44005a7f34481d421256409000000"
                                }],
                            }
                        }))),
                )
                .await;
        (mock_ecrow_subgraph_server, _mock_ecrow_subgraph)
    }
    #[bon::builder]
    async fn create_sender_allocation_args(
        pgpool: PgPool,
        sender_aggregator_endpoint: Option<String>,
        escrow_subgraph_endpoint: &str,
        #[builder(default = 1000)] rav_request_receipt_limit: u64,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> SenderAllocationArgs {
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
            )
            .await,
        ));

        let escrow_accounts_rx = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let sender_account_ref = match sender_account {
            Some(sender) => sender,
            None => create_mock_sender_account().await.1,
        };

        let aggregator_url = match sender_aggregator_endpoint {
            Some(url) => url,
            None => get_grpc_url().await,
        };

        let endpoint = Endpoint::new(aggregator_url).unwrap();

        let sender_aggregator = TapAggregatorClient::connect(endpoint.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "Failed to connect to the TapAggregator endpoint '{}': Err: {err:?}",
                    endpoint.uri()
                )
            });

        SenderAllocationArgs::builder()
            .pgpool(pgpool.clone())
            .allocation_id(ALLOCATION_ID_0)
            .sender(SENDER.1)
            .escrow_accounts(escrow_accounts_rx)
            .escrow_subgraph(escrow_subgraph)
            .domain_separator(TAP_EIP712_DOMAIN_SEPARATOR.clone())
            .sender_account_ref(sender_account_ref)
            .sender_aggregator(sender_aggregator)
            .config(super::AllocationConfig {
                timestamp_buffer_ns: 1,
                rav_request_receipt_limit,
                indexer_address: INDEXER.1,
                escrow_polling_interval: Duration::from_millis(1000),
            })
            .build()
    }

    #[bon::builder]
    async fn create_sender_allocation(
        pgpool: PgPool,
        sender_aggregator_endpoint: Option<String>,
        escrow_subgraph_endpoint: &str,
        #[builder(default = 1000)] rav_request_receipt_limit: u64,
        sender_account: Option<ActorRef<SenderAccountMessage>>,
    ) -> (ActorRef<SenderAllocationMessage>, Arc<Notify>) {
        let args = create_sender_allocation_args()
            .pgpool(pgpool)
            .maybe_sender_aggregator_endpoint(sender_aggregator_endpoint)
            .escrow_subgraph_endpoint(escrow_subgraph_endpoint)
            .sender_account(sender_account.unwrap())
            .rav_request_receipt_limit(rav_request_receipt_limit)
            .call()
            .await;
        let actor = TestableActor::new(SenderAllocation);
        let notify = actor.notify.clone();

        let (allocation_ref, _join_handle) = Actor::spawn(None, actor, args).await.unwrap();

        (allocation_ref, notify)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn should_update_unaggregated_fees_on_start(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let (mut last_message_emitted, sender_account) = create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Should emit a message to the sender account with the unaggregated fees.
        let expected_message = SenderAccountMessage::UpdateReceiptFees(
            ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                last_id: 10,
                value: 55u128,
                counter: 10,
            }),
        );
        let last_message_emitted = last_message_emitted.recv().await.unwrap();
        assert_eq!(last_message_emitted, expected_message);

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 55u128);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn should_return_invalid_receipts_on_startup(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;
        // Add receipts to the database.
        for i in 1..=10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_invalid_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Get total_unaggregated_fees
        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Should emit a message to the sender account with the unaggregated fees.
        let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
            ALLOCATION_ID_0,
            UnaggregatedReceipts {
                last_id: 10,
                value: 55u128,
                counter: 10,
            },
        );
        let update_invalid_msg = message_receiver.recv().await.unwrap();
        assert_eq!(update_invalid_msg, expected_message);
        let last_message_emitted = message_receiver.recv().await.unwrap();
        assert_eq!(
            last_message_emitted,
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
            )
        );

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn test_receive_new_receipt(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        let (sender_allocation, notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // should validate with id less than last_id
        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 0,
                value: 10,
                allocation_id: ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns: 0,
            })
        )
        .unwrap();

        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        cast!(
            sender_allocation,
            SenderAllocationMessage::NewReceipt(NewReceiptNotification {
                id: 1,
                value: 20,
                allocation_id: ALLOCATION_ID_0,
                signer_address: SIGNER.1,
                timestamp_ns,
            })
        )
        .unwrap();

        flush_messages(&notify).await;

        // should emit update aggregate fees message to sender account
        let expected_message = SenderAccountMessage::UpdateReceiptFees(
            ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(20u128, timestamp_ns),
        );
        let startup_load_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_load_msg,
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 0,
                    last_id: 0,
                    counter: 0,
                })
            )
        );
        let last_message_emitted = message_receiver.recv().await.unwrap();
        assert_eq!(last_message_emitted, expected_message);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_trigger_rav_request(pgpool: PgPool) {
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
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&notify).await;

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
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 90,
                    last_id: 20,
                    counter: 20,
                })
            )
        );

        // Check if the sender received invalid receipt fees
        let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
            ALLOCATION_ID_0,
            UnaggregatedReceipts {
                last_id: 0,
                value: 45,
                counter: 0,
            },
        );
        assert_eq!(message_receiver.recv().await.unwrap(), expected_message);

        assert!(matches!(
            message_receiver.recv().await.unwrap(),
            SenderAccountMessage::UpdateReceiptFees(_, ReceiptFees::RavRequestResponse(_))
        ));
    }

    async fn execute<Fut>(
        pgpool: PgPool,
        amount_of_receipts: u64,
        populate: impl FnOnce(PgPool) -> Fut,
    ) where
        Fut: Future<Output = ()>,
    {
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

        populate(pgpool.clone()).await;

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .rav_request_receipt_limit(2000)
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&notify).await;

        let total_unaggregated_fees = call!(
            sender_allocation,
            SenderAllocationMessage::GetUnaggregatedReceipts
        )
        .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);

        let startup_msg = message_receiver.recv().await.unwrap();
        let expected_value: u128 = ((amount_of_receipts.to_u128().expect("should be u128")
            - 1u128)
            * (amount_of_receipts.to_u128().expect("should be u128")))
            / 2;
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: expected_value,
                    last_id: amount_of_receipts,
                    counter: amount_of_receipts,
                })
            )
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_several_receipts_rav_request(pgpool: PgPool) {
        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        execute(pgpool, AMOUNT_OF_RECEIPTS, |pgpool| async move {
            // Add receipts to the database.

            for i in 0..AMOUNT_OF_RECEIPTS {
                let receipt =
                    create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
                store_receipt(&pgpool, receipt.signed_receipt())
                    .await
                    .unwrap();
            }
        })
        .await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_several_receipts_batch_insert_rav_request(pgpool: PgPool) {
        // Add batch receipts to the database.
        const AMOUNT_OF_RECEIPTS: u64 = 1000;
        execute(pgpool, AMOUNT_OF_RECEIPTS, |pgpool| async move {
            // Add receipts to the database.
            let mut receipts = Vec::with_capacity(1000);
            for i in 0..AMOUNT_OF_RECEIPTS {
                let receipt =
                    create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
                receipts.push(receipt);
            }
            let res = store_batch_receipts(&pgpool, receipts).await;
            assert!(res.is_ok());
        })
        .await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_close_allocation_no_pending_fees(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // create allocation
        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        // check if message is sent to sender account
        assert_eq!(
            message_receiver.recv().await.unwrap(),
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
            )
        );
    }

    // used for test_close_allocation_with_pending_fees(pgpool:
    mod wiremock_gen {
        wiremock_grpc::generate!("tap_aggregator.v1.TapAggregator", MockTapAggregator);
    }

    #[test_log::test(sqlx::test(migrations = "../../migrations"))]
    async fn test_close_allocation_with_pending_fees(pgpool: PgPool) {
        use wiremock_gen::MockTapAggregator;
        let mut mock_aggregator = MockTapAggregator::start_default().await;

        let request1 = mock_aggregator.setup(
            MockBuilder::when()
                //    👇 RPC prefix
                .path("/tap_aggregator.v1.TapAggregator/AggregateReceipts")
                .then()
                .return_status(Code::Ok)
                .return_body(|| {
                    let mock_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 10, 45);
                    RavResponse {
                        rav: Some(mock_rav.into()),
                    }
                }),
        );

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

        let (_, sender_account) = create_mock_sender_account().await;

        // create allocation
        let (sender_allocation, _notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .sender_aggregator_endpoint(format!(
                "http://[::1]:{}",
                mock_aggregator.address().port()
            ))
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        sender_allocation.stop_and_wait(None, None).await.unwrap();

        // check if rav request was made
        assert!(mock_aggregator.find_request_count() > 0);
        assert!(mock_aggregator.find(&request1).is_some());

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn should_return_unaggregated_fees_without_rav(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
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
        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn should_calculate_invalid_receipts_fee(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
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
    #[sqlx::test(migrations = "../../migrations")]
    async fn should_return_unaggregated_fees_with_rav(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // Add the RAV to the database.
        // This RAV has timestamp 4. The sender_allocation should only consider receipts
        // with a timestamp greater than 4.
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 35u128);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_store_failed_rav(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);

        // just unit test if it is working
        let result = state
            .store_failed_rav(&signed_rav.message, &signed_rav, "test")
            .await;

        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_store_invalid_receipts(pgpool: PgPool) {
        struct FailingCheck;

        #[async_trait::async_trait]
        impl Check<TapReceipt> for FailingCheck {
            async fn check(
                &self,
                _: &tap_core::receipt::Context,
                _receipt: &CheckingReceipt,
            ) -> CheckResult {
                Err(CheckError::Failed(anyhow::anyhow!("Failing check")))
            }
        }

        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
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
                    .finalize_receipt_checks(&Context::new(), &checks)
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_mark_rav_last(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
        let signed_rav = create_rav(ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        let args = create_sender_allocation_args()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .call()
            .await;
        let state = SenderAllocationState::new(args).await.unwrap();

        // mark rav as final
        let result = state.mark_rav_last().await;

        // check if it fails
        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_failed_rav_request(pgpool: PgPool) {
        let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, u64::MAX, i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        // Create a sender_allocation.
        let (sender_allocation, notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_escrow_subgraph_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&notify).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 45,
                    last_id: 10,
                    counter: 10,
                })
            )
        );
        let rav_response_message = message_receiver.recv().await.unwrap();
        match rav_response_message {
            SenderAccountMessage::UpdateReceiptFees(
                _,
                ReceiptFees::RavRequestResponse(rav_response),
            ) => {
                assert!(rav_response.1.is_err());
            }
            v => panic!("Expecting RavRequestResponse as last message, found: {v:?}"),
        }

        // expect the actor to keep running
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

        // Check that the unaggregated fees return the same value
        // TODO: Maybe this can no longer be checked?
        //assert_eq!(total_unaggregated_fees.value, 45u128);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_rav_request_when_all_receipts_invalid(pgpool: PgPool) {
        // Start a mock graphql server using wiremock
        let mock_server = MockServer::start().await;

        // Mock result for TAP redeem txs for (allocation, sender) pair.
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({ "data": { "transactions": [
                            {
                                "id": "redeemed"
                            }
                        ]}}),
                    )),
            )
            .await;
        // Add invalid receipts to the database. ( already redeemed )
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos() as u64;
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

        let (mut message_receiver, sender_account) = create_mock_sender_account().await;

        let (sender_allocation, notify) = create_sender_allocation()
            .pgpool(pgpool.clone())
            .escrow_subgraph_endpoint(&mock_server.uri())
            .sender_account(sender_account)
            .call()
            .await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        sender_allocation
            .cast(SenderAllocationMessage::TriggerRavRequest)
            .unwrap();

        flush_messages(&notify).await;

        // If it is an error then rav request failed

        let startup_msg = message_receiver.recv().await.unwrap();
        assert_eq!(
            startup_msg,
            SenderAccountMessage::UpdateReceiptFees(
                ALLOCATION_ID_0,
                ReceiptFees::UpdateValue(UnaggregatedReceipts {
                    value: 16220184412847561580,
                    last_id: 10,
                    counter: 10,
                })
            )
        );

        let invalid_receipts = message_receiver.recv().await.unwrap();

        assert_eq!(
            invalid_receipts,
            SenderAccountMessage::UpdateInvalidReceiptFees(
                ALLOCATION_ID_0,
                UnaggregatedReceipts {
                    value: TOTAL_SUM,
                    last_id: 0,
                    counter: 0,
                }
            )
        );

        let rav_response_message = message_receiver.recv().await.unwrap();
        match rav_response_message {
            SenderAccountMessage::UpdateReceiptFees(
                _,
                ReceiptFees::RavRequestResponse(rav_response),
            ) => {
                assert!(rav_response.1.is_err());
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
