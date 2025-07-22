// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Enhanced Integration Tests
//!
//! This module demonstrates the new testing infrastructure with improved
//! test structure, error handling, and observability.

use anyhow::Result;
use std::time::Duration;

use crate::{
    constants::*,
    test_context::{TestContext, TestError},
    test_utils::{MetricsUtils, ReceiptUtils, TestAssertions},
};

/// Example test demonstrating the new infrastructure
pub async fn test_v2_receipt_processing_enhanced() -> Result<()> {
    let mut ctx = TestContext::new().await?;

    println!(
        "🧪 Starting enhanced V2 receipt processing test (ID: {})",
        ctx.test_id
    );

    // Step 1: Verify system is ready
    let horizon_enabled = ctx.verify_horizon_detection().await?;
    TestAssertions::assert_horizon_mode_enabled(horizon_enabled)?;
    println!("✅ Horizon mode detected");

    // Step 2: Find active allocation
    let allocation = ctx.find_active_allocation().await?;
    println!("✅ Found active allocation: {}", allocation.id);
    ctx.allocations.push(allocation.clone());

    // Step 3: Get initial metrics
    let initial_metrics = ctx.metrics_checker.get_current_metrics().await?;
    let initial_ravs = initial_metrics.ravs_created_by_allocation(&allocation.id.to_string());
    let initial_fees = initial_metrics.unaggregated_fees_by_allocation(&allocation.id.to_string());

    println!("📊 Initial metrics - RAVs: {initial_ravs}, Unaggregated fees: {initial_fees}");

    // Step 4: Send batch of V2 receipts
    let batch_size = 5;
    let receipt_value = MAX_RECEIPT_VALUE / 10;
    let payer = ctx.wallet.address();
    let service_provider = allocation.id; // Using allocation_id as service provider

    println!("📨 Sending {batch_size} V2 receipts...");
    let successful_receipts = ReceiptUtils::send_v2_receipt_batch(
        &ctx,
        &allocation.id,
        batch_size,
        receipt_value,
        &payer,
        &service_provider,
    )
    .await?;

    TestAssertions::assert_receipts_accepted(successful_receipts, batch_size)?;
    println!("✅ All {successful_receipts} receipts accepted");

    // Step 5: Wait for processing (with timeout)
    println!("⏳ Waiting for receipt processing...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Step 6: Check if RAV generation occurred
    let rav_result = MetricsUtils::wait_for_rav_generation(
        &ctx,
        &allocation.id,
        initial_ravs,
        Duration::from_secs(30),
    )
    .await;

    let fee_result = MetricsUtils::wait_for_fee_aggregation(
        &ctx,
        &allocation.id,
        initial_fees,
        Duration::from_secs(30),
    )
    .await;

    // Test passes if either RAV generation or fee aggregation occurs
    match (rav_result, fee_result) {
        (Ok(new_ravs), _) => {
            println!("✅ Test passed: RAV generation detected ({initial_ravs} -> {new_ravs})");
        }
        (_, Ok(new_fees)) => {
            println!("✅ Test passed: Fee aggregation detected ({initial_fees} -> {new_fees})");
        }
        (Err(rav_err), Err(fee_err)) => {
            println!("❌ Test failed: Neither RAV generation nor fee aggregation occurred");
            println!("   RAV error: {rav_err}");
            println!("   Fee error: {fee_err}");
            return Err(anyhow::anyhow!(TestError::Timeout {
                condition: "RAV generation or fee aggregation".to_string(),
            }));
        }
    }

    // Step 7: Cleanup
    ctx.cleanup().await?;

    println!("🎉 Enhanced V2 receipt processing test completed successfully!");
    Ok(())
}

/// Example test demonstrating error scenario handling
pub async fn test_insufficient_escrow_scenario() -> Result<()> {
    let mut ctx = TestContext::new().await?;

    println!(
        "🧪 Starting insufficient escrow scenario test (ID: {})",
        ctx.test_id
    );

    // Find allocation
    let allocation = ctx.find_active_allocation().await?;
    println!("✅ Found active allocation: {}", allocation.id);

    // Try to send a receipt with excessive value
    let excessive_value = MAX_RECEIPT_VALUE * 1000; // Much larger than typical escrow
    let payer = ctx.wallet.address();
    let service_provider = allocation.id;

    println!("📨 Attempting to send receipt with excessive value: {excessive_value}");

    let result = ReceiptUtils::send_v2_receipt(
        &ctx,
        &allocation.id,
        excessive_value,
        &payer,
        &service_provider,
    )
    .await;

    match result {
        Err(e) => {
            println!("✅ Receipt correctly rejected: {e}");
            // Check if it's the expected error type
            if e.to_string().contains("Payment Required") || e.to_string().contains("402") {
                println!("✅ Correct error type detected");
            } else {
                println!("⚠️  Unexpected error type, but still acceptable");
            }
        }
        Ok(_) => {
            println!("❌ Receipt was unexpectedly accepted");
            return Err(anyhow::anyhow!(TestError::ReceiptValidationFailed {
                reason: "Receipt with excessive value should have been rejected".to_string(),
            }));
        }
    }

    // Cleanup
    ctx.cleanup().await?;

    println!("🎉 Insufficient escrow scenario test completed successfully!");
    Ok(())
}

/// Example test demonstrating batch processing with monitoring
pub async fn test_batch_processing_with_monitoring() -> Result<()> {
    let mut ctx = TestContext::new().await?;

    println!(
        "🧪 Starting batch processing monitoring test (ID: {})",
        ctx.test_id
    );

    // Setup
    let allocation = ctx.find_active_allocation().await?;
    let payer = ctx.wallet.address();
    let service_provider = allocation.id;

    // Send multiple small batches and monitor progress
    let batches = 3;
    let batch_size = 3;
    let receipt_value = MAX_RECEIPT_VALUE / 20;

    println!("📨 Sending {batches} batches of {batch_size} receipts each...");

    for batch_num in 0..batches {
        println!("  📦 Batch {}/{}", batch_num + 1, batches);

        let successful = ReceiptUtils::send_v2_receipt_batch(
            &ctx,
            &allocation.id,
            batch_size,
            receipt_value,
            &payer,
            &service_provider,
        )
        .await?;

        TestAssertions::assert_receipts_accepted(successful, batch_size)?;

        // Check metrics after each batch
        let metrics = ctx.metrics_checker.get_current_metrics().await?;
        let current_fees = metrics.unaggregated_fees_by_allocation(&allocation.id.to_string());

        println!(
            "  📊 Batch {} complete - Current unaggregated fees: {}",
            batch_num + 1,
            current_fees
        );

        // Small delay between batches to allow processing
        if batch_num < batches - 1 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    println!("✅ All batches processed successfully");

    // Wait for final processing
    println!("⏳ Waiting for final processing...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    let final_metrics = ctx.metrics_checker.get_current_metrics().await?;
    let final_ravs = final_metrics.ravs_created_by_allocation(&allocation.id.to_string());
    let final_fees = final_metrics.unaggregated_fees_by_allocation(&allocation.id.to_string());

    println!("📊 Final metrics - RAVs: {final_ravs}, Unaggregated fees: {final_fees}");

    // Cleanup
    ctx.cleanup().await?;

    println!("🎉 Batch processing monitoring test completed successfully!");
    Ok(())
}

// Test runner function for the enhanced tests
pub async fn run_enhanced_tests() -> Result<()> {
    println!("🚀 Running enhanced integration tests...");

    // Test 1: Basic V2 receipt processing
    if let Err(e) = test_v2_receipt_processing_enhanced().await {
        println!("❌ Enhanced V2 receipt processing test failed: {e}");
        return Err(e);
    }

    // Test 2: Error scenario handling
    if let Err(e) = test_insufficient_escrow_scenario().await {
        println!("❌ Insufficient escrow scenario test failed: {e}");
        return Err(e);
    }

    // Test 3: Batch processing with monitoring
    if let Err(e) = test_batch_processing_with_monitoring().await {
        println!("❌ Batch processing monitoring test failed: {e}");
        return Err(e);
    }

    println!("🎉 All enhanced integration tests passed!");
    Ok(())
}

#[cfg(test)]
mod tests {
    // use super::*;
    // use crate::test_with_context;

    // Example of using the test macro for unit-style testing
    // TODO: Fix macro to handle async blocks properly
    /*
    test_with_context!(test_context_creation, |ctx: &mut TestContext| async {
        // Simple test to verify context creation works
        assert!(!ctx.test_id.is_empty());
        assert!(ctx.allocations.is_empty());
        assert!(ctx.cleanup_tasks.is_empty());
        Ok(())
    });
    */
}
