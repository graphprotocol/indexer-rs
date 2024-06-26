// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::hex::ToHex;
use alloy_sol_types::Eip712Domain;
use anyhow::{anyhow, ensure, Result};
use bigdecimal::num_bigint::BigInt;
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use prometheus::{
    register_counter, register_counter_vec, register_gauge_vec, register_histogram_vec, Counter,
    CounterVec, GaugeVec, HistogramVec,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::{
    manager::adapters::RAVRead,
    rav::{RAVRequest, ReceiptAggregateVoucher, SignedRAV},
    receipt::{
        checks::{Check, Checks},
        Failed, ReceiptWithState,
    },
    signed_message::EIP712SignedMessage,
};
use thegraph::types::Address;
use tracing::{error, warn};

use crate::lazy_static;

use crate::agent::sender_account::SenderAccountMessage;
use crate::agent::sender_accounts_manager::NewReceiptNotification;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::{
    config::{self},
    tap::context::{checks::Signature, TapAgentContext},
    tap::signers_trimmed,
    tap::{context::checks::AllocationId, escrow_adapter::EscrowAdapter},
};

lazy_static! {
    static ref UNAGGREGATED_FEES: GaugeVec = register_gauge_vec!(
        format!("unaggregated_fees"),
        "Unggregated Fees value",
        &["sender", "allocation"]
    )
    .unwrap();
}

lazy_static! {
    static ref RAV_VALUE: GaugeVec = register_gauge_vec!(
        format!("rav_value"),
        "Value of the last RAV",
        &["sender", "allocation"]
    )
    .unwrap();
}

lazy_static! {
    static ref CLOSED_SENDER_ALLOCATIONS: Counter = register_counter!(
        format!("closed_sender_allocation"),
        "Count of sender-allocation managers closed since the start of the program",
    )
    .unwrap();
}

lazy_static! {
    static ref RAVS_CREATED: CounterVec = register_counter_vec!(
        format!("ravs_created"),
        "RAVs updated or created per sender allocation since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
}

lazy_static! {
    static ref RAVS_FAILED: CounterVec = register_counter_vec!(
        format!("ravs_failed"),
        "RAV requests failed since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
}

lazy_static! {
    static ref RAV_RESPONSE_TIME: HistogramVec = register_histogram_vec!(
        format!("rav_response_time"),
        "RAV response time per sender",
        &["sender"]
    )
    .unwrap();
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
    sender_aggregator_endpoint: String,
    config: &'static config::Config,
    escrow_accounts: Eventual<EscrowAccounts>,
    domain_separator: Eip712Domain,
    sender_account_ref: ActorRef<SenderAccountMessage>,
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
    pub sender_aggregator_endpoint: String,
    pub sender_account_ref: ActorRef<SenderAccountMessage>,
}

#[derive(Debug)]
pub enum SenderAllocationMessage {
    NewReceipt(NewReceiptNotification),
    TriggerRAVRequest(RpcReplyPort<(UnaggregatedReceipts, Option<SignedRAV>)>),
    #[cfg(test)]
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
        args: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let sender_account_ref = args.sender_account_ref.clone();
        let allocation_id = args.allocation_id;
        let mut state = SenderAllocationState::new(args).await;

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
            state.unaggregated_fees.clone(),
        ))?;

        UNAGGREGATED_FEES
            .with_label_values(&[&state.sender.to_string(), &state.allocation_id.to_string()])
            .set(state.unaggregated_fees.value as f64);

        // update rav tracker for sender account
        if let Some(rav) = &state.latest_rav {
            sender_account_ref.cast(SenderAccountMessage::UpdateRav(rav.clone()))?;

            RAV_VALUE
                .with_label_values(&[&state.sender.to_string(), &state.allocation_id.to_string()])
                .set(rav.message.valueAggregate as f64);
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
        CLOSED_SENDER_ALLOCATIONS.inc();

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
                    // it's fine to crash the actor, could not send a message to its parent
                    state
                        .sender_account_ref
                        .cast(SenderAccountMessage::UpdateReceiptFees(
                            state.allocation_id,
                            unaggreated_fees.clone(),
                        ))?;
                }
            }
            // we use a blocking call here to ensure that only one RAV request is running at a time.
            SenderAllocationMessage::TriggerRAVRequest(reply) => {
                if state.unaggregated_fees.value > 0 {
                    // auto backoff retry, on error ignore
                    let _ = state.request_rav().await;
                }
                if !reply.is_closed() {
                    let _ = reply.send((state.unaggregated_fees.clone(), state.latest_rav.clone()));
                }
            }
            #[cfg(test)]
            SenderAllocationMessage::GetUnaggregatedReceipts(reply) => {
                if !reply.is_closed() {
                    let _ = reply.send(unaggreated_fees.clone());
                }
            }
        }

        // We expect the value to change for every received receipt, and after every RAV request.
        UNAGGREGATED_FEES
            .with_label_values(&[&state.sender.to_string(), &state.allocation_id.to_string()])
            .set(state.unaggregated_fees.value as f64);

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
            sender_aggregator_endpoint,
            sender_account_ref,
        }: SenderAllocationArgs,
    ) -> Self {
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
            Checks::new(required_checks),
        );

        Self {
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
            invalid_receipts_fees: UnaggregatedReceipts::default(),
            latest_rav,
        }
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn calculate_unaggregated_fee(&self) -> Result<UnaggregatedReceipts> {
        tracing::trace!("calculate_unaggregated_fee()");
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
            self.allocation_id.encode_hex::<String>(),
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
        let mut retries = 0;
        const MAX_RETRIES: u32 = 3;
        while retries < MAX_RETRIES {
            match self.rav_requester_single().await {
                Ok(rav) => {
                    self.unaggregated_fees = self.calculate_unaggregated_fee().await?;
                    self.latest_rav = Some(rav);
                    return Ok(());
                }
                Err(e) => {
                    error!(
                        "Error while requesting RAV for sender {} and allocation {}: {}",
                        self.sender, self.allocation_id, e
                    );
                    RAVS_FAILED
                        .with_label_values(&[
                            &self.sender.to_string(),
                            &self.allocation_id.to_string(),
                        ])
                        .inc();
                    // backoff = 100ms * 2 ^ retries
                    tokio::time::sleep(Duration::from_millis(100) * 2u32.pow(retries)).await;
                    retries += 1;
                }
            }
        }
        Err(anyhow!("Could not finish rav request"))
    }

    /// Request a RAV from the sender's TAP aggregator. Only one RAV request will be running at a
    /// time through the use of an internal guard.
    async fn rav_requester_single(&mut self) -> Result<SignedRAV> {
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
            self.store_invalid_receipts(invalid_receipts.as_slice())
                .await?;
        }
        let client = HttpClientBuilder::default()
            .request_timeout(Duration::from_secs(
                self.config.tap.rav_request_timeout_secs,
            ))
            .build(&self.sender_aggregator_endpoint)?;
        let rav_response_time_start = Instant::now();
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

        let rav_response_time = rav_response_time_start.elapsed();
        RAV_RESPONSE_TIME
            .with_label_values(&[&self.sender.to_string()])
            .observe(rav_response_time.as_secs_f64());

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
        RAV_VALUE
            .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
            .set(expected_rav.clone().valueAggregate as f64);
        RAVS_CREATED
            .with_label_values(&[&self.sender.to_string(), &self.allocation_id.to_string()])
            .inc();

        Ok(response.data)
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
            self.allocation_id.encode_hex::<String>(),
            self.sender.encode_hex::<String>(),
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
            .map_err(|e| anyhow!("Failed to store invalid receipt: {:?}", e))?;
        }
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
pub mod tests {
    use super::{
        SenderAllocation, SenderAllocationArgs, SenderAllocationMessage, SenderAllocationState,
    };
    use crate::{
        agent::{
            sender_account::SenderAccountMessage, sender_accounts_manager::NewReceiptNotification,
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
    use ractor::{
        call, cast, concurrency::JoinHandle, Actor, ActorProcessingErr, ActorRef, ActorStatus,
    };
    use serde_json::json;
    use sqlx::PgPool;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    use tap_aggregator::{jsonrpsee_helpers::JsonRpcResponse, server::run_server};
    use tap_core::receipt::{
        checks::{Check, Checks},
        Checking, ReceiptWithState,
    };
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, Respond, ResponseTemplate,
    };

    const DUMMY_URL: &str = "http://localhost:1234";

    pub struct MockSenderAccount {
        pub last_message_emitted: Arc<Mutex<Vec<SenderAccountMessage>>>,
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
            self.last_message_emitted.lock().unwrap().push(message);
            Ok(())
        }
    }

    async fn create_mock_sender_account() -> (
        Arc<Mutex<Vec<SenderAccountMessage>>>,
        ActorRef<SenderAccountMessage>,
        JoinHandle<()>,
    ) {
        let last_message_emitted = Arc::new(Mutex::new(vec![]));

        let (sender_account, join_handle) = MockSenderAccount::spawn(
            None,
            MockSenderAccount {
                last_message_emitted: last_message_emitted.clone(),
            },
            (),
        )
        .await
        .unwrap();
        (last_message_emitted, sender_account, join_handle)
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
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint, None).unwrap(),
        )));

        let escrow_accounts_eventual = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone(), SENDER.1);

        let sender_account_ref = match sender_account {
            Some(sender) => sender,
            None => create_mock_sender_account().await.1,
        };

        SenderAllocationArgs {
            config,
            pgpool: pgpool.clone(),
            allocation_id: *ALLOCATION_ID_0,
            sender: SENDER.1,
            escrow_accounts: escrow_accounts_eventual,
            escrow_subgraph,
            escrow_adapter,
            domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint,
            sender_account_ref,
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
        let (last_message_emitted, sender_account, _join_handle) =
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
            UnaggregatedReceipts {
                last_id: 10,
                value: 55u128,
            },
        );
        let last_message_emitted = last_message_emitted.lock().unwrap();
        assert_eq!(last_message_emitted.len(), 1);
        assert_eq!(last_message_emitted.last(), Some(&expected_message));

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 55u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_return_invalid_receipts_on_startup(pgpool: PgPool) {
        let (last_message_emitted, sender_account, _join_handle) =
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
        let last_message_emitted = last_message_emitted.lock().unwrap();
        assert_eq!(last_message_emitted.len(), 2);
        assert_eq!(last_message_emitted.first(), Some(&expected_message));

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_receive_new_receipt(pgpool: PgPool) {
        let (last_message_emitted, sender_account, _join_handle) =
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
            UnaggregatedReceipts {
                last_id: 1,
                value: 20,
            },
        );
        let last_message_emitted = last_message_emitted.lock().unwrap();
        assert_eq!(last_message_emitted.len(), 2);
        assert_eq!(last_message_emitted.last(), Some(&expected_message));
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

        let (last_message_emitted, sender_account, _join_handle) =
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
        let (total_unaggregated_fees, _rav) = call!(
            sender_allocation,
            SenderAllocationMessage::TriggerRAVRequest
        )
        .unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(total_unaggregated_fees.value, 0u128);

        // Check if the sender received invalid receipt fees
        let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
            *ALLOCATION_ID_0,
            UnaggregatedReceipts {
                last_id: 0,
                value: 45u128,
            },
        );
        {
            let last_message_emitted = last_message_emitted.lock().unwrap();
            assert_eq!(last_message_emitted.last(), Some(&expected_message));
        }

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_close_allocation_no_pending_fees(pgpool: PgPool) {
        let (last_message_emitted, sender_account, _join_handle) =
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
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // check if the actor is actually stopped
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        // check if message is sent to sender account
        assert_eq!(
            last_message_emitted.lock().unwrap().last(),
            Some(&SenderAccountMessage::UpdateReceiptFees(
                *ALLOCATION_ID_0,
                UnaggregatedReceipts::default()
            ))
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
        let state = SenderAllocationState::new(args).await;

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
        let state = SenderAllocationState::new(args).await;

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
        let state = SenderAllocationState::new(args).await;

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
        let state = SenderAllocationState::new(args).await;

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
            async fn check(&self, _receipt: &ReceiptWithState<Checking>) -> anyhow::Result<()> {
                Err(anyhow::anyhow!("Failing check"))
            }
        }

        let args =
            create_sender_allocation_args(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None)
                .await;
        let mut state = SenderAllocationState::new(args).await;

        let checks = Checks::new(vec![Arc::new(FailingCheck)]);

        // create some checks
        let checking_receipts = vec![
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
        ];
        // make sure to fail them
        let failing_receipts = checking_receipts
            .into_iter()
            .map(|receipt| async { receipt.finalize_receipt_checks(&checks).await.unwrap_err() })
            .collect::<Vec<_>>();
        let failing_receipts = join_all(failing_receipts).await;

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
        let state = SenderAllocationState::new(args).await;

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
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, u64::max_value(), i.into());
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Create a sender_allocation.
        let sender_allocation =
            create_sender_allocation(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL, None).await;

        // Trigger a RAV request manually and wait for updated fees.
        // this should fail because there's no receipt with valid timestamp
        let (total_unaggregated_fees, _rav) = call!(
            sender_allocation,
            SenderAllocationMessage::TriggerRAVRequest
        )
        .unwrap();

        // expect the actor to keep running
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

        // Check that the unaggregated fees return the same value
        assert_eq!(total_unaggregated_fees.value, 45u128);
    }
}
