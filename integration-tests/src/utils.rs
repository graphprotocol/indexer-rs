// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Result;
use base64::prelude::*;
use prost::Message;
use rand::{rng, Rng};
use reqwest::Client;
use serde_json::json;
use tap_aggregator::grpc;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::Receipt;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};
use thegraph_core::CollectionId;

use crate::constants::{
    GATEWAY_API_KEY, GATEWAY_URL, GRAPH_TALLY_COLLECTOR_CONTRACT, SUBGRAPH_ID, TEST_DATA_SERVICE,
};

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

pub fn create_tap_receipt_v2(
    value: u128,
    allocation_id: &Address,  // Used to derive collection_id in V2
    _verifier_contract: &str, // V2 uses GraphTallyCollector, not TAPVerifier
    chain_id: u64,
    wallet: &PrivateKeySigner,
    payer: &Address,
    service_provider: &Address,
) -> Result<Eip712SignedMessage<tap_graph::v2::Receipt>> {
    let nonce = rng().random::<u64>();

    // Get timestamp in nanoseconds
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let timestamp_ns = timestamp as u64;

    // In V2, convert the allocation_id to a collection_id
    // For the migration period, we derive collection_id from allocation_id
    let collection_id = CollectionId::from(*allocation_id);

    // Create domain separator - V2 uses GraphTallyCollector
    let eip712_domain_separator =
        tap_eip712_domain(chain_id, Address::from_str(GRAPH_TALLY_COLLECTOR_CONTRACT)?);

    let wallet_address = wallet.address();
    // Create and sign V2 receipt
    println!("Creating and signing V2 receipt...");
    println!("V2 Receipt details:");
    println!("  Payer (from wallet): {payer:?}");
    println!("  Service provider: {service_provider:?}");
    println!("  Data service: {TEST_DATA_SERVICE}");
    println!("  Collection ID: {collection_id:?}");
    println!("  Wallet address: {wallet_address:?}");
    println!("  Using GraphTallyCollector: {GRAPH_TALLY_COLLECTOR_CONTRACT}");

    let receipt = Eip712SignedMessage::new(
        &eip712_domain_separator,
        tap_graph::v2::Receipt {
            collection_id: *collection_id,
            payer: *payer,
            service_provider: *service_provider,
            data_service: Address::from_str(TEST_DATA_SERVICE)?, // Use proper data service
            nonce,
            timestamp_ns,
            value,
        },
        wallet,
    )?;

    Ok(receipt)
}

pub fn encode_v2_receipt(receipt: &Eip712SignedMessage<tap_graph::v2::Receipt>) -> Result<String> {
    let protobuf_receipt = grpc::v2::SignedReceipt::from(receipt.clone());
    let encoded = protobuf_receipt.encode_to_vec();
    let base64_encoded = BASE64_STANDARD.encode(encoded);
    Ok(base64_encoded)
}

// Function to create a configured request (currently unused - kept for compatibility)
#[allow(dead_code)]
pub fn create_request(
    client: &reqwest::Client,
    url: &str,
    receipt_json: &str,
    query: &serde_json::Value,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("Content-Type", "application/json")
        .header("tap-receipt", receipt_json)
        .json(query)
        .timeout(Duration::from_secs(10))
}

// Function to create a configured request with gateway API key
pub fn create_request_with_api_key(
    client: &reqwest::Client,
    url: &str,
    receipt_json: &str,
    query: &serde_json::Value,
    api_key: &str,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("Content-Type", "application/json")
        .header("tap-receipt", receipt_json)
        .header("Authorization", format!("Bearer {api_key}"))
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

pub async fn verify_gateway_routing() -> Result<()> {
    println!("🔍 Verifying gateway routing configuration...");

    let client = Arc::new(Client::new());

    // Test 1: Verify gateway is responding
    println!("📡 Testing gateway availability...");
    let gateway_response = client
        .get(format!("{GATEWAY_URL}/health"))
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    match gateway_response {
        Ok(resp) => {
            if resp.status().is_success() {
                println!("✅ Gateway is responding at {GATEWAY_URL}");
            } else {
                println!("⚠️  Gateway responded with status: {}", resp.status());
            }
        }
        Err(e) => {
            println!("❌ Gateway not reachable: {e}");
            return Err(anyhow::anyhow!("Gateway not reachable: {}", e));
        }
    }

    // Test 2: Try a simple GraphQL query through the gateway
    println!("📊 Testing subgraph query routing through gateway...");
    let query_response = client
        .post(format!("{GATEWAY_URL}/api/subgraphs/id/{SUBGRAPH_ID}"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {GATEWAY_API_KEY}"))
        .json(&json!({
            "query": "{ _meta { block { number } } }"
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    let status = query_response.status();
    let response_text = query_response.text().await?;

    println!("📋 Gateway response status: {status}");
    println!("📋 Gateway response body: {response_text}");

    if status.is_success() {
        println!("✅ Gateway successfully routed query to subgraph {SUBGRAPH_ID}");
    } else if status == 404 && response_text.contains("subgraph not found") {
        println!(
            "❌ Gateway routing issue: Subgraph {SUBGRAPH_ID} not found in gateway's routing table"
        );
        println!(
            "💡 This confirms the root cause - gateway cannot route to the specified subgraph ID"
        );
        return Err(anyhow::anyhow!(
            "Gateway routing verification failed: subgraph not found"
        ));
    } else {
        println!("⚠️  Unexpected gateway response: {status} - {response_text}");
    }

    println!("🎯 Gateway routing verification complete!");
    Ok(())
}
