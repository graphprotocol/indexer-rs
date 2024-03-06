// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use alloy_primitives::hex::ToHex;
use alloy_sol_types::Eip712Domain;
use anyhow::{anyhow, ensure, Result};
use bigdecimal::num_bigint::BigInt;
use eventuals::Eventual;
use indexer_common::{escrow_accounts::EscrowAccounts, prelude::SubgraphClient};
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::jsonrpsee_helpers::JsonRpcResponse;
use tap_core::checks::{Check, TimestampCheck};
use tap_core::{
    eip_712_signed_message::EIP712SignedMessage,
    receipt_aggregate_voucher::ReceiptAggregateVoucher, tap_manager::RAVRequest,
    tap_receipt::ReceivedReceipt,
};
use thegraph::types::Address;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, warn};

use crate::{
    config::{self},
    tap::{signers_trimmed, unaggregated_receipts::UnaggregatedReceipts},
};

use super::executor::{checks::Signature, TapAgentExecutor};
use super::{escrow_adapter::EscrowAdapter, executor::checks::AllocationId};

type TapManager = tap_core::tap_manager::Manager<TapAgentExecutor>;

/// Manages unaggregated fees and the TAP lifecyle for a specific (allocation, sender) pair.
pub struct SenderAllocation {
    pgpool: PgPool,
    tap_manager: TapManager,
    allocation_id: Address,
    sender: Address,
    sender_aggregator_endpoint: String,
    unaggregated_fees: Arc<StdMutex<UnaggregatedReceipts>>,
    config: &'static config::Cli,
    escrow_accounts: Eventual<EscrowAccounts>,
    rav_request_guard: TokioMutex<()>,
    unaggregated_receipts_guard: TokioMutex<()>,
}

impl SenderAllocation {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_subgraph: &'static SubgraphClient,
        escrow_adapter: EscrowAdapter,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoint: String,
    ) -> Self {
        let timestamp_check = Arc::new(TimestampCheck::new(0));
        let required_checks: Vec<Arc<dyn Check>> = vec![
            timestamp_check.clone(),
            Arc::new(AllocationId::new(
                sender,
                allocation_id,
                escrow_subgraph,
                &config,
            )),
            Arc::new(Signature::new(
                tap_eip712_domain_separator.clone(),
                escrow_accounts.clone(),
            )),
        ];
        let executor = TapAgentExecutor::new(
            pgpool.clone(),
            allocation_id,
            sender,
            required_checks.clone(),
            escrow_accounts.clone(),
            escrow_adapter,
            timestamp_check,
        );
        let tap_manager = TapManager::new(
            tap_eip712_domain_separator.clone(),
            executor,
            required_checks,
        );

        let sender_allocation = Self {
            pgpool,
            tap_manager,
            allocation_id,
            sender,
            sender_aggregator_endpoint,
            unaggregated_fees: Arc::new(StdMutex::new(UnaggregatedReceipts::default())),
            config,
            escrow_accounts,
            rav_request_guard: TokioMutex::new(()),
            unaggregated_receipts_guard: TokioMutex::new(()),
        };

        sender_allocation
            .update_unaggregated_fees()
            .await
            .map_err(|e| {
                error!(
                    "Error while updating unaggregated fees for allocation {}: {}",
                    allocation_id, e
                )
            })
            .ok();

        sender_allocation
    }

    /// Delete obsolete receipts in the DB w.r.t. the last RAV in DB, then update the tap manager
    /// with the latest unaggregated fees from the database.
    async fn update_unaggregated_fees(&self) -> Result<()> {
        // Make sure to pause the handling of receipt notifications while we update the unaggregated
        // fees.
        let _guard = self.unaggregated_receipts_guard.lock().await;

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

        *self.unaggregated_fees.lock().unwrap() = UnaggregatedReceipts {
            last_id: res.max.unwrap_or(0).try_into()?,
            value: res
                .sum
                .unwrap_or(BigDecimal::from(0))
                .to_string()
                .parse::<u128>()?,
        };

        // TODO: check if we need to run a RAV request here.

        Ok(())
    }

    /// Request a RAV from the sender's TAP aggregator. Only one RAV request will be running at a
    /// time through the use of an internal guard.
    pub async fn rav_requester_single(&self) -> Result<()> {
        // Making extra sure that only one RAV request is running at a time.
        let _guard = self.rav_request_guard.lock().await;

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
            .await?;
        if !invalid_receipts.is_empty() {
            warn!(
                "Found {} invalid receipts for allocation {} and sender {}.",
                invalid_receipts.len(),
                self.allocation_id,
                self.sender
            );

            // Save invalid receipts to the database for logs.
            // TODO: consider doing that in a spawned task?
            Self::store_invalid_receipts(
                self,
                &invalid_receipts
                    .into_iter()
                    .map(|invalid| invalid.into())
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
            .await?;
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
        let escrow_accounts = self.escrow_accounts.clone();
        let expected_sender = self.sender.clone();
        match self
            .tap_manager
            .verify_and_store_rav(
                expected_rav.clone(),
                response.data.clone(),
                |signer| async move {
                    let escrow_account = escrow_accounts.value().await.map_err(|_| {
                        tap_core::Error::InvalidCheckError {
                            check_string: "Could not load escrow_accounts eventual".into(),
                        }
                    })?;
                    let sender = escrow_account.get_sender_for_signer(&signer).map_err(|_| {
                        tap_core::Error::InvalidCheckError {
                            check_string: format!(
                                "Could not find the sender for the signer {}",
                                signer
                            ),
                        }
                    })?;
                    Ok(sender == expected_sender)
                },
            )
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
        Self::update_unaggregated_fees(self).await?;
        Ok(())
    }

    pub async fn mark_rav_final(&self) -> Result<()> {
        let updated_rows = sqlx::query!(
            r#"
                        UPDATE scalar_tap_ravs
                        SET final = true
                        WHERE allocation_id = $1 AND sender_address = $2
                        RETURNING *
                    "#,
            self.allocation_id.encode_hex::<String>(),
            self.sender.encode_hex::<String>(),
        )
        .fetch_all(&self.pgpool)
        .await?;
        if updated_rows.len() != 1 {
            anyhow::bail!(
                "Expected exactly one row to be updated in the latest RAVs table, \
                        but {} were updated.",
                updated_rows.len()
            );
        };
        Ok(())
    }

    async fn store_invalid_receipts(&self, receipts: &[ReceivedReceipt]) -> Result<()> {
        for received_receipt in receipts.iter() {
            sqlx::query!(
                r#"
                    INSERT INTO scalar_tap_receipts_invalid (
                        allocation_id,
                        signer_address,
                        timestamp_ns,
                        value,
                        received_receipt
                    )
                    VALUES ($1, $2, $3, $4, $5)
                "#,
                self.allocation_id.encode_hex::<String>(),
                self.sender.encode_hex::<String>(),
                BigDecimal::from(received_receipt.signed_receipt().message.timestamp_ns),
                BigDecimal::from(BigInt::from(
                    received_receipt.signed_receipt().message.value
                )),
                serde_json::to_value(received_receipt)?
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

    /// Safe add the fees to the unaggregated fees value if the receipt_id is greater than the
    /// last_id. If the addition would overflow u128, log an error and set the unaggregated fees
    /// value to u128::MAX.
    ///
    /// Returns true if the fees were added, false otherwise.
    pub async fn fees_add(&self, fees: u128, receipt_id: u64) -> bool {
        // Make sure to pause the handling of receipt notifications while we update the unaggregated
        // fees.
        let _guard = self.unaggregated_receipts_guard.lock().await;

        let mut fees_added = false;
        let mut unaggregated_fees = self.unaggregated_fees.lock().unwrap();

        if receipt_id > unaggregated_fees.last_id {
            *unaggregated_fees = UnaggregatedReceipts {
                last_id: receipt_id,
                value: unaggregated_fees
                    .value
                    .checked_add(fees)
                    .unwrap_or_else(|| {
                        // This should never happen, but if it does, we want to know about it.
                        error!(
                            "Overflow when adding receipt value {} to total unaggregated fees {} \
                            for allocation {} and sender {}. Setting total unaggregated fees to \
                            u128::MAX.",
                            fees, unaggregated_fees.value, self.allocation_id, self.sender
                        );
                        u128::MAX
                    }),
            };
            fees_added = true;
        }

        fees_added
    }

    pub fn get_unaggregated_fees(&self) -> UnaggregatedReceipts {
        self.unaggregated_fees.lock().unwrap().clone()
    }

    pub fn get_allocation_id(&self) -> Address {
        self.allocation_id
    }
}

#[cfg(test)]
mod tests {

    use std::collections::{HashMap, HashSet};

    use indexer_common::subgraph_client::DeploymentDetails;
    use serde_json::json;
    use tap_aggregator::server::run_server;

    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::tap::test_utils::{
        create_rav, create_received_receipt, store_rav, store_receipt, ALLOCATION_ID_0, INDEXER,
        SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    };

    const DUMMY_URL: &str = "http://localhost:1234";

    async fn create_sender_allocation(
        pgpool: PgPool,
        sender_aggregator_endpoint: String,
        escrow_subgraph_endpoint: &str,
    ) -> SenderAllocation {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: 100,
                rav_request_timestamp_buffer_ms: 1,
                rav_request_timeout_secs: 5,
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
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let escrow_adapter = EscrowAdapter::new(escrow_accounts_eventual.clone());

        SenderAllocation::new(
            config,
            pgpool.clone(),
            *ALLOCATION_ID_0,
            SENDER.1,
            escrow_accounts_eventual,
            escrow_subgraph,
            escrow_adapter,
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_aggregator_endpoint,
        )
        .await
    }

    /// Test that the sender_allocation correctly updates the unaggregated fees from the
    /// database when there is no RAV in the database.
    ///
    /// The sender_allocation should consider all receipts found for the allocation and
    /// sender.
    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_unaggregated_fees_no_rav(pgpool: PgPool) {
        let sender_allocation =
            create_sender_allocation(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

        // Add receipts to the database.
        for i in 1..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation update the unaggregated fees from the database.
        sender_allocation.update_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation.unaggregated_fees.lock().unwrap().value,
            45u128
        );
    }

    /// Test that the sender_allocation correctly updates the unaggregated fees from the
    /// database when there is a RAV in the database as well as receipts which timestamp are lesser
    /// and greater than the RAV's timestamp.
    ///
    /// The sender_allocation should only consider receipts with a timestamp greater
    /// than the RAV's timestamp.
    #[sqlx::test(migrations = "../migrations")]
    async fn test_update_unaggregated_fees_with_rav(pgpool: PgPool) {
        let sender_allocation =
            create_sender_allocation(pgpool.clone(), DUMMY_URL.to_string(), DUMMY_URL).await;

        // Add the RAV to the database.
        // This RAV has timestamp 4. The sender_allocation should only consider receipts
        // with a timestamp greater than 4.
        let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10).await;
        store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

        // Add receipts to the database.
        for i in 1..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation update the unaggregated fees from the database.
        sender_allocation.update_unaggregated_fees().await.unwrap();

        // Check that the unaggregated fees are correct.
        assert_eq!(
            sender_allocation.unaggregated_fees.lock().unwrap().value,
            35u128
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rav_requester_manual(pgpool: PgPool) {
        // Start a TAP aggregator server.
        let (handle, aggregator_endpoint) = run_server(
            0,
            SIGNER.0.clone(),
            HashSet::new(),
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

        // Create a sender_allocation.
        let sender_allocation = create_sender_allocation(
            pgpool.clone(),
            "http://".to_owned() + &aggregator_endpoint.to_string(),
            &mock_server.uri(),
        )
        .await;

        // Add receipts to the database.
        for i in 0..10 {
            let receipt =
                create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into(), i).await;
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        // Let the sender_allocation update the unaggregated fees from the database.
        sender_allocation.update_unaggregated_fees().await.unwrap();

        // Trigger a RAV request manually.
        sender_allocation.rav_requester_single().await.unwrap();

        // Stop the TAP aggregator server.
        handle.stop().unwrap();
        handle.stopped().await;
    }
}
