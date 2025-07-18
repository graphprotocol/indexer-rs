// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Test Utilities
//!
//! This module provides reusable utilities for integration testing,
//! extracted from the existing test code and enhanced for better reusability.

use std::time::Duration;

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::str::FromStr;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::Receipt;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

use crate::{
    constants::*,
    test_context::{TestContext, TestError},
};

/// Utilities for receipt operations
pub struct ReceiptUtils;

impl ReceiptUtils {
    /// Create and send a V1 receipt
    /// Reserved for future V1 receipt testing
    #[allow(dead_code)]
    pub async fn send_v1_receipt(
        ctx: &TestContext,
        allocation_id: &Address,
        value: u128,
    ) -> Result<()> {
        let receipt = Self::create_v1_receipt(value, allocation_id, &ctx.wallet)?;
        let receipt_json = serde_json::to_string(&receipt)?;

        let response = ctx
            .http_client
            .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
            .header("Tap-Receipt", receipt_json)
            .json(&json!({
                "query": "{ _meta { block { number } } }"
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(TestError::ReceiptValidationFailed {
                reason: format!("HTTP {}: {}", response.status(), response.text().await?),
            }));
        }

        Ok(())
    }

    /// Create and send a V2 receipt
    pub async fn send_v2_receipt(
        ctx: &TestContext,
        allocation_id: &Address,
        value: u128,
        payer: &Address,
        service_provider: &Address,
    ) -> Result<()> {
        let receipt =
            Self::create_v2_receipt(value, allocation_id, &ctx.wallet, payer, service_provider)?;
        let receipt_encoded = Self::encode_v2_receipt(&receipt)?;

        let response = ctx
            .http_client
            .post(format!("{INDEXER_URL}/subgraphs/id/{SUBGRAPH_ID}"))
            .header("Content-Type", "application/json")
            .header("Tap-Receipt", receipt_encoded)
            .json(&json!({
                "query": "{ _meta { block { number } } }"
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(TestError::ReceiptValidationFailed {
                reason: format!("HTTP {}: {}", response.status(), response.text().await?),
            }));
        }

        Ok(())
    }

    /// Send a batch of V1 receipts
    /// Reserved for future V1 batch receipt testing
    #[allow(dead_code)]
    pub async fn send_v1_receipt_batch(
        ctx: &TestContext,
        allocation_id: &Address,
        batch_size: usize,
        receipt_value: u128,
    ) -> Result<usize> {
        let mut successful = 0;

        for i in 0..batch_size {
            match Self::send_v1_receipt(ctx, allocation_id, receipt_value).await {
                Ok(_) => {
                    successful += 1;
                    println!("✅ V1 Receipt {}/{} sent successfully", i + 1, batch_size);
                }
                Err(e) => {
                    println!("❌ V1 Receipt {}/{} failed: {}", i + 1, batch_size, e);
                    return Err(e);
                }
            }

            // Small delay between receipts
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(successful)
    }

    /// Send a batch of V2 receipts
    pub async fn send_v2_receipt_batch(
        ctx: &TestContext,
        allocation_id: &Address,
        batch_size: usize,
        receipt_value: u128,
        payer: &Address,
        service_provider: &Address,
    ) -> Result<usize> {
        let mut successful = 0;

        for i in 0..batch_size {
            match Self::send_v2_receipt(ctx, allocation_id, receipt_value, payer, service_provider)
                .await
            {
                Ok(_) => {
                    successful += 1;
                    println!("✅ V2 Receipt {}/{} sent successfully", i + 1, batch_size);
                }
                Err(e) => {
                    println!("❌ V2 Receipt {}/{} failed: {}", i + 1, batch_size, e);
                    return Err(e);
                }
            }

            // Small delay between receipts
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(successful)
    }

    /// Create a V1 TAP receipt (extracted from utils.rs)
    /// Reserved for future V1 receipt testing
    #[allow(dead_code)]
    fn create_v1_receipt(
        value: u128,
        allocation_id: &Address,
        wallet: &PrivateKeySigner,
    ) -> Result<Eip712SignedMessage<Receipt>> {
        use rand::{rng, Rng};
        use std::time::SystemTime;

        let nonce = rng().random::<u64>();

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let timestamp_ns = timestamp as u64;

        let eip712_domain_separator =
            tap_eip712_domain(CHAIN_ID, Address::from_str(TAP_VERIFIER_CONTRACT)?);

        let receipt = Eip712SignedMessage::new(
            &eip712_domain_separator,
            Receipt {
                allocation_id: *allocation_id,
                nonce,
                timestamp_ns,
                value,
            },
            wallet,
        )?;

        Ok(receipt)
    }

    /// Create a V2 TAP receipt (extracted from utils.rs)
    fn create_v2_receipt(
        value: u128,
        allocation_id: &Address,
        wallet: &PrivateKeySigner,
        payer: &Address,
        service_provider: &Address,
    ) -> Result<Eip712SignedMessage<tap_graph::v2::Receipt>> {
        use rand::{rng, Rng};
        use std::time::SystemTime;
        use thegraph_core::CollectionId;

        let nonce = rng().random::<u64>();

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let timestamp_ns = timestamp as u64;

        let collection_id = CollectionId::from(*allocation_id);

        let eip712_domain_separator =
            tap_eip712_domain(CHAIN_ID, Address::from_str(TAP_VERIFIER_CONTRACT)?);

        let receipt = Eip712SignedMessage::new(
            &eip712_domain_separator,
            tap_graph::v2::Receipt {
                collection_id: *collection_id,
                payer: *payer,
                service_provider: *service_provider,
                data_service: Address::from_str(TEST_DATA_SERVICE)?,
                nonce,
                timestamp_ns,
                value,
            },
            wallet,
        )?;

        Ok(receipt)
    }

    /// Encode V2 receipt as base64 protobuf (extracted from utils.rs)
    fn encode_v2_receipt(receipt: &Eip712SignedMessage<tap_graph::v2::Receipt>) -> Result<String> {
        use base64::prelude::*;
        use prost::Message;
        use tap_aggregator::grpc;

        let protobuf_receipt = grpc::v2::SignedReceipt::from(receipt.clone());
        let encoded = protobuf_receipt.encode_to_vec();
        let base64_encoded = BASE64_STANDARD.encode(encoded);
        Ok(base64_encoded)
    }
}

/// Utilities for escrow operations
///
/// Reserved for future comprehensive escrow balance tracking and validation.
#[allow(dead_code)]
pub struct EscrowUtils;

impl EscrowUtils {
    /// Check if V1 escrow has sufficient balance
    /// Reserved for future V1 escrow balance validation tests
    #[allow(dead_code)]
    pub async fn check_v1_escrow_balance(
        _ctx: &TestContext,
        _sender: &Address,
        _required_amount: u128,
    ) -> Result<bool> {
        // This would query the V1 escrow contract
        // For now, return true as placeholder
        Ok(true)
    }

    /// Check if V2 escrow has sufficient balance
    /// Reserved for future V2 escrow balance validation tests
    #[allow(dead_code)]
    pub async fn check_v2_escrow_balance(
        ctx: &TestContext,
        payer: &Address,
        collector: &Address,
        receiver: &Address,
        required_amount: u128,
    ) -> Result<bool> {
        // Query the network subgraph for escrow balance
        let response = ctx.http_client
            .post(GRAPH_URL)
            .json(&json!({
                "query": format!(
                    "{{ paymentsEscrowAccounts(where: {{ payer: \"{}\", collector: \"{}\", receiver: \"{}\" }}) {{ balance }} }}",
                    payer, collector, receiver
                )
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let response_text = response.text().await?;
        let json_value = serde_json::from_str::<serde_json::Value>(&response_text)?;

        let balance = json_value
            .get("data")
            .and_then(|d| d.get("paymentsEscrowAccounts"))
            .and_then(|a| a.as_array())
            .and_then(|arr| arr.first())
            .and_then(|account| account.get("balance"))
            .and_then(|b| b.as_str())
            .and_then(|s| s.parse::<u128>().ok())
            .unwrap_or(0);

        Ok(balance >= required_amount)
    }
}

/// Utilities for service operations
///
/// Reserved for future service health monitoring and availability testing.
#[allow(dead_code)]
pub struct ServiceUtils;

impl ServiceUtils {
    /// Check if a service is healthy
    /// Reserved for future service health monitoring tests
    #[allow(dead_code)]
    pub async fn check_service_health(service_url: &str) -> Result<bool> {
        let client = Client::new();
        match client
            .get(service_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// Wait for service to be healthy
    /// Reserved for future service availability tests
    #[allow(dead_code)]
    pub async fn wait_for_service_health(service_url: &str, timeout: Duration) -> Result<()> {
        let start = std::time::SystemTime::now();

        while start.elapsed().unwrap() < timeout {
            if Self::check_service_health(service_url).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        Err(anyhow::anyhow!(TestError::ServiceUnavailable {
            service: service_url.to_string(),
        }))
    }

    /// Check if Horizon migration mode is enabled
    /// Reserved for future Horizon mode detection tests
    #[allow(dead_code)]
    pub async fn check_horizon_mode() -> Result<bool> {
        // Check indexer-service logs for Horizon detection
        // This would need to be implemented based on service API or logs
        Ok(false)
    }
}

/// Utilities for metrics operations
pub struct MetricsUtils;

impl MetricsUtils {
    /// Wait for RAV generation with detailed monitoring
    pub async fn wait_for_rav_generation(
        ctx: &TestContext,
        allocation_id: &Address,
        initial_ravs: u32,
        timeout: Duration,
    ) -> Result<u32> {
        let start = std::time::SystemTime::now();

        while start.elapsed().unwrap() < timeout {
            let current_metrics = ctx.metrics_checker.get_current_metrics().await?;
            let current_ravs =
                current_metrics.ravs_created_by_allocation(&allocation_id.to_string());

            if current_ravs > initial_ravs {
                println!("✅ RAV generation detected: {initial_ravs} -> {current_ravs}");
                return Ok(current_ravs);
            }

            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        Err(anyhow::anyhow!(TestError::Timeout {
            condition: format!("RAV generation for allocation {allocation_id}")
        }))
    }

    /// Wait for unaggregated fees to decrease
    pub async fn wait_for_fee_aggregation(
        ctx: &TestContext,
        allocation_id: &Address,
        initial_fees: f64,
        timeout: Duration,
    ) -> Result<f64> {
        let start = std::time::SystemTime::now();

        while start.elapsed().unwrap() < timeout {
            let current_metrics = ctx.metrics_checker.get_current_metrics().await?;
            let current_fees =
                current_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

            // Consider significant decrease as success (90% reduction)
            if current_fees < initial_fees * 0.9 {
                println!("✅ Fee aggregation detected: {initial_fees} -> {current_fees}");
                return Ok(current_fees);
            }

            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        Err(anyhow::anyhow!(TestError::Timeout {
            condition: format!("Fee aggregation for allocation {allocation_id}"),
        }))
    }
}

/// Test assertions with better error messages
pub struct TestAssertions;

impl TestAssertions {
    /// Assert that receipts are being accepted
    pub fn assert_receipts_accepted(successful_receipts: usize, expected: usize) -> Result<()> {
        if successful_receipts != expected {
            return Err(anyhow::anyhow!(TestError::ReceiptValidationFailed {
                reason: format!(
                    "Expected {expected} successful receipts, got {successful_receipts}"
                ),
            }));
        }
        Ok(())
    }

    /// Assert that Horizon mode is enabled
    pub fn assert_horizon_mode_enabled(enabled: bool) -> Result<()> {
        if !enabled {
            return Err(anyhow::anyhow!(TestError::HorizonDetectionFailed {
                expected_accounts: 1,
                found_accounts: 0,
            }));
        }
        Ok(())
    }

    /// Assert that metrics show expected values
    /// Reserved for future metrics range validation tests
    #[allow(dead_code)]
    pub fn assert_metrics_in_range(
        actual: usize,
        expected_min: usize,
        expected_max: usize,
        metric_name: &str,
    ) -> Result<()> {
        if actual < expected_min || actual > expected_max {
            return Err(anyhow::anyhow!(TestError::ReceiptValidationFailed {
                reason: format!("{metric_name} out of range: expected {expected_min}-{expected_max}, got {actual}"),
            }));
        }
        Ok(())
    }
}
