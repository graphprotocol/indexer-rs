use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thegraph_core::alloy::signers::local::coins_bip39::English;
use thegraph_core::alloy::{
    primitives::Address,
    signers::local::{MnemonicBuilder, PrivateKeySigner},
};

mod metrics;
mod receipt;
use metrics::MetricsChecker;
use receipt::create_tap_receipt;

// Constants taken from local-network/.env
// it could be possible to read that file
// along with the local-network/contracts.json
// and regex over to get bellow values
const GATEWAY_URL: &str = "http://localhost:7700";
const SUBGRAPH_ID: &str = "BFr2mx7FgkJ36Y6pE5BiXs1KmNUmVDCnL82KUSdcLW1g";
const TAP_ESCROW_CONTRACT: &str = "0x0355B7B8cb128fA5692729Ab3AAa199C1753f726";
const GATEWAY_API_KEY: &str = "deadbeefdeadbeefdeadbeefdeadbeef";
const RECEIVER_ADDRESS: &str = "0xf4EF6650E48d099a4972ea5B414daB86e1998Bd3";
const TAP_AGENT_METRICS_URL: &str = "http://localhost:7300/metrics";

#[tokio::main]
async fn main() -> Result<()> {
    // Run the TAP receipt test
    tap_rav_test().await
}

async fn tap_rav_test() -> Result<()> {
    // Setup wallet using your MnemonicBuilder
    let index: u32 = 0;
    let wallet: PrivateKeySigner = MnemonicBuilder::<English>::default()
        .phrase("test test test test test test test test test test test junk")
        .index(index)
        .unwrap()
        .build()
        .unwrap();

    let sender_address = wallet.address();
    println!("Using sender address: {}", sender_address);

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Query the network subgraph to find active allocations
    println!("Querying for active allocations...");
    let allocations_query = http_client
        .post("http://localhost:8000/subgraphs/name/graph-network")
        .json(&json!({
            "query": "{ allocations(where: { status: Active }) { id indexer { id } subgraphDeployment { id } } }"
        }))
        .send()
        .await;

    // Default to a fallback allocation ID
    let mut allocation_id = Address::from_str("0x0000000000000000000000000000000000000000")?;

    // Try to find a valid allocation
    if let Ok(response) = allocations_query {
        if response.status().is_success() {
            let response_text = response.text().await.unwrap();
            println!("Network subgraph response: {}", response_text);

            if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Some(allocations) = json_value
                    .get("data")
                    .and_then(|d| d.get("allocations"))
                    .and_then(|a| a.as_array())
                {
                    if !allocations.is_empty() {
                        if let Some(id_str) = allocations[0].get("id").and_then(|id| id.as_str()) {
                            println!("Found allocation ID: {}", id_str);
                            allocation_id = Address::from_str(id_str)?;
                        }
                    }
                }
            }
        }
    }

    // If we still don't have an allocation ID, create a mock one based on the receiver address
    if allocation_id == Address::from_str("0x0000000000000000000000000000000000000000")? {
        println!("No allocation found, using a mock allocation based on receiver address");
        allocation_id = Address::from_str(RECEIVER_ADDRESS)?;
    }

    // Create a metrics checker
    let metrics_checker =
        MetricsChecker::new(http_client.clone(), TAP_AGENT_METRICS_URL.to_string());

    let initial_metrics = metrics_checker.get_current_metrics().await?;
    // Extract the initial metrics we care about
    // in this case the number of created/requested ravs
    // and the amount of unaggregated fees
    // so later at the end of the test we compare them
    // to see if we triggered any RAV generation
    let initial_ravs_created =
        initial_metrics.ravs_created_by_allocation(&allocation_id.to_string());
    let initial_unaggregated =
        initial_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

    // The value for each receipt
    let value = 100_000_000_000_000u128; // 0.0001 GRT

    // With trigger_value_divisor = 10,000 and max_amount_willing_to_lose_grt = 20
    // trigger_value = 20 / 10,000 = 0.002 GRT
    // We need to send at least 20 receipts to reach the trigger threshold
    // Sending slightly more than required to ensure triggering
    let num_receipts = 60;

    println!(
        "\n=== Sending {} receipts to trigger RAV generation ===",
        num_receipts
    );
    println!(
        "Each receipt value: {} GRT",
        value as f64 / 1_000_000_000_000_000f64
    );
    println!(
        "Total value to be sent: {} GRT",
        (value as f64 * num_receipts as f64) / 1_000_000_000_000_000f64
    );

    // let mut trigger_value = 0.0;

    let trigger_value = initial_metrics.trigger_value_by_sender(&sender_address.to_string());

    if trigger_value > 0.0 {
        println!(
            "With trigger value of {} GRT, we need to send at least {} receipts",
            trigger_value,
            (trigger_value * 1_000_000_000_000_000f64 / value as f64).ceil()
        );
    }

    // Send receipts in batches with a delay in between
    // to ensure some receipts get outside the timestamp buffer
    let batches = 3;
    let receipts_per_batch = num_receipts / batches;
    let mut total_successful = 0;

    for batch in 0..batches {
        println!(
            "Sending batch {} of {} ({} receipts per batch)",
            batch + 1,
            batches,
            receipts_per_batch
        );

        for i in 0..receipts_per_batch {
            let receipt_index = batch * receipts_per_batch + i;
            println!("Sending receipt {} of {}", receipt_index + 1, num_receipts);

            // Create TAP receipt
            let receipt = create_tap_receipt(value, &allocation_id, TAP_ESCROW_CONTRACT, &wallet)?;
            let receipt_json = serde_json::to_string(&receipt).unwrap();

            let query_response = http_client
                .post(format!("{}/api/subgraphs/id/{}", GATEWAY_URL, SUBGRAPH_ID))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", GATEWAY_API_KEY))
                .header("Tap-Receipt", receipt_json)
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .timeout(Duration::from_secs(10))
                .send()
                .await;

            match query_response {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        total_successful += 1;
                        println!("Receipt {} sent successfully", receipt_index + 1);
                    } else {
                        println!("Failed to send receipt {}: {}", receipt_index + 1, status);
                        let response_text = response.text().await?;
                        println!("Response: {}", response_text);
                    }
                }
                Err(e) => {
                    println!("Error sending receipt {}: {}", receipt_index + 1, e);
                    return Err(e.into());
                }
            }

            // Small delay between queries to avoid flooding
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // After each batch, wait longer than the timestamp buffer
        // (typically 60 seconds) to ensure receipts are outside buffer
        if batch < batches - 1 {
            println!(
                "\nBatch {} complete. Waiting 65 seconds to exceed timestamp buffer...",
                batch + 1
            );
            tokio::time::sleep(Duration::from_secs(65)).await;
        }
    }

    println!("\n=== Summary ===");
    println!(
        "Total receipts sent successfully: {}/{}",
        total_successful, num_receipts
    );
    println!(
        "Total value sent: {} GRT",
        (value as f64 * total_successful as f64) / 1_000_000_000_000_000f64
    );

    // Check for RAV generation
    println!("\n=== Checking for RAV generation ===");

    // Get final metrics
    println!("Getting final metrics snapshot...");
    let final_metrics = metrics_checker.get_current_metrics().await?;

    // Extract the final metrics
    let final_ravs_created = final_metrics.ravs_created_by_allocation(&allocation_id.to_string());
    let final_unaggregated =
        final_metrics.unaggregated_fees_by_allocation(&allocation_id.to_string());

    println!(
        "Final state: RAVs created: {}, Unaggregated fees: {}",
        final_ravs_created, final_unaggregated
    );

    // Check for success criteria
    if final_ravs_created > initial_ravs_created {
        println!(
            "✅ TEST PASSED: RAVs created increased from {} to {}!",
            initial_ravs_created, final_ravs_created
        );
        return Ok(());
    }

    if final_unaggregated < initial_unaggregated * 0.9 {
        println!(
            "✅ TEST PASSED: Unaggregated fees decreased significantly from {} to {}!",
            initial_unaggregated, final_unaggregated
        );
        return Ok(());
    }

    // If we got here, test failed
    println!("❌ TEST FAILED: No RAV generation detected");

    Err(anyhow::anyhow!("Failed to detect RAV generation"))
}
