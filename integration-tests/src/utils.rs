// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use rand::{rng, Rng};
use serde_json::json;
use std::time::{Duration, SystemTime};
use std::{str::FromStr, sync::Arc};

use anyhow::Result;
use reqwest::Client;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::Receipt;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

pub fn create_tap_receipt(
    value: u128,
    allocation_id: &Address,
    verifier_contract: &str,
    chain_id: u64,
    wallet: &PrivateKeySigner,
) -> Result<Eip712SignedMessage<Receipt>> {
    let nonce = rng().random::<u64>();

    // Get timestamp in nanoseconds
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let timestamp_ns = timestamp as u64;

    // Create domain separator
    let eip712_domain_separator =
        tap_eip712_domain(chain_id, Address::from_str(verifier_contract)?);

    // Create and sign receipt
    println!("Creating and signing receipt...");
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

// Function to create a configured request
pub fn create_request(
    client: &reqwest::Client,
    url: &str,
    receipt_json: &str,
    query: &serde_json::Value,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Tap-Receipt", receipt_json)
        .json(query)
        .timeout(Duration::from_secs(10))
}

pub async fn find_allocation(http_client: Arc<Client>, url: &str) -> Result<String> {
    println!("Querying for active allocations...");
    let response = http_client
        .post(url)
        .json(&json!({
            "query": "{ allocations(where: { status: Active }) { id indexer { id } subgraphDeployment { id } } }"
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Network subgraph request failed with status: {}",
            response.status()
        ));
    }

    // Try to find a valid allocation
    let response_text = response.text().await?;

    let json_value = serde_json::from_str::<serde_json::Value>(&response_text)?;
    json_value
        .get("data")
        .and_then(|d| d.get("allocations"))
        .and_then(|a| a.as_array())
        .filter(|arr| !arr.is_empty())
        .and_then(|arr| arr[0].get("id"))
        .and_then(|id| id.as_str())
        .map(|id| id.to_string())
        .ok_or_else(|| anyhow::anyhow!("No valid allocation ID found"))
}
