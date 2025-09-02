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
use rdkafka::{
    config::ClientConfig,
    producer::{BaseRecord, DefaultProducerContext, ThreadedProducer},
};
use reqwest::Client;
use serde_json::json;
use tap_aggregator::grpc;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::Receipt;
use thegraph_core::alloy::{
    primitives::{Address, U256},
    signers::local::PrivateKeySigner,
};
use thegraph_core::CollectionId;

use crate::constants::{GRAPH_TALLY_COLLECTOR_CONTRACT, TEST_DATA_SERVICE};

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
    let eip712_domain_separator = tap_eip712_domain(
        chain_id,
        Address::from_str(verifier_contract)?,
        tap_core::TapVersion::V1,
    );

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
    let eip712_domain_separator = tap_eip712_domain(
        chain_id,
        Address::from_str(GRAPH_TALLY_COLLECTOR_CONTRACT)?,
        tap_core::TapVersion::V2,
    );

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
    println!(
        "***Received response from network subgraph {}",
        response_text
    );

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

/// Gateway-style Receipt Signer (similar to gateway_palaver's ReceiptSigner)
pub struct GatewayReceiptSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
    verifying_contract: Address,
}

impl GatewayReceiptSigner {
    pub fn new(signer: PrivateKeySigner, chain_id: U256, verifying_contract: Address) -> Self {
        Self {
            signer,
            chain_id: chain_id.as_limbs()[0],
            verifying_contract,
        }
    }

    /// Create a v2 receipt (collection-based) following the working V2 test approach
    pub fn create_receipt(
        &self,
        collection: CollectionId,
        fee: u128,
        payer: Address,
        data_service: Address,
        service_provider: Address,
    ) -> Result<Eip712SignedMessage<tap_graph::v2::Receipt>> {
        let nonce = rng().random::<u64>();
        let timestamp_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .map_err(|_| anyhow::anyhow!("failed to convert timestamp to ns"))?;

        // let timestamp_ns = SystemTime::now()
        //     .duration_since(SystemTime::UNIX_EPOCH)?
        //     .as_nanos() as u64;

        // Use the same domain creation as working V2 tests
        let eip712_domain_separator = tap_eip712_domain(
            self.chain_id,
            self.verifying_contract,
            tap_core::TapVersion::V2,
        );

        let receipt = tap_graph::v2::Receipt {
            collection_id: collection.0.into(),
            payer,
            data_service,
            service_provider,
            timestamp_ns,
            nonce,
            value: fee,
        };

        let signed = Eip712SignedMessage::new(&eip712_domain_separator, receipt, &self.signer)?;
        Ok(signed)
    }

    pub fn payer_address(&self) -> Address {
        self.signer.address()
    }
}

/// Encode a v2 receipt for the Tap-Receipt header
pub fn encode_v2_receipt_for_header(
    receipt: &Eip712SignedMessage<tap_graph::v2::Receipt>,
) -> Result<String> {
    let protobuf_receipt = grpc::v2::SignedReceipt::from(receipt.clone());
    let encoded = protobuf_receipt.encode_to_vec();
    let base64_encoded = BASE64_STANDARD.encode(encoded);
    Ok(base64_encoded)
}

/// Protobuf structures for Kafka reporting (mirroring gateway_palaver's structures)
#[derive(prost::Message)]
pub struct ClientQueryProtobuf {
    #[prost(string, tag = "1")]
    pub gateway_id: String,
    // 20 bytes
    #[prost(bytes, tag = "2")]
    pub receipt_signer: Vec<u8>,
    #[prost(string, tag = "3")]
    pub query_id: String,
    #[prost(string, tag = "4")]
    pub api_key: String,
    #[prost(string, tag = "11")]
    pub user_id: String,
    #[prost(string, optional, tag = "12")]
    pub subgraph: Option<String>,
    #[prost(string, tag = "5")]
    pub result: String,
    #[prost(uint32, tag = "6")]
    pub response_time_ms: u32,
    #[prost(uint32, tag = "7")]
    pub request_bytes: u32,
    #[prost(uint32, optional, tag = "8")]
    pub response_bytes: Option<u32>,
    #[prost(double, tag = "9")]
    pub total_fees_usd: f64,
    #[prost(message, repeated, tag = "10")]
    pub indexer_queries: Vec<IndexerQueryProtobuf>,
}

#[derive(prost::Message)]
pub struct IndexerQueryProtobuf {
    /// 20 bytes
    #[prost(bytes, tag = "1")]
    pub indexer: Vec<u8>,
    /// 32 bytes
    #[prost(bytes, tag = "2")]
    pub deployment: Vec<u8>,
    /// 20 bytes - Allocation ID for v1 receipts
    #[prost(bytes, optional, tag = "3")]
    pub allocation: Option<Vec<u8>>,
    /// 32 bytes - Collection ID for v2 receipts
    #[prost(bytes, optional, tag = "12")]
    pub collection: Option<Vec<u8>>,
    #[prost(string, tag = "4")]
    pub indexed_chain: String,
    #[prost(string, tag = "5")]
    pub url: String,
    #[prost(double, tag = "6")]
    pub fee_grt: f64,
    #[prost(uint32, tag = "7")]
    pub response_time_ms: u32,
    #[prost(uint32, tag = "8")]
    pub seconds_behind: u32,
    #[prost(string, tag = "9")]
    pub result: String,
    #[prost(string, tag = "10")]
    pub indexer_errors: String,
    #[prost(uint64, tag = "11")]
    pub blocks_behind: u64,
}

/// Kafka producer wrapper
pub struct KafkaReporter {
    producer: ThreadedProducer<DefaultProducerContext>,
    write_buf: Vec<u8>,
}

impl KafkaReporter {
    /// Create a new Kafka reporter with bootstrap servers
    pub fn new(bootstrap_servers: &str) -> Result<Self> {
        let producer: ThreadedProducer<DefaultProducerContext> = ClientConfig::new()
            .set("bootstrap.servers", bootstrap_servers)
            .set("message.timeout.ms", "5000")
            .create()?;

        Ok(Self {
            producer,
            write_buf: Vec::new(),
        })
    }

    /// Publish a message to the specified Kafka topic
    pub fn publish_to_topic<T: Message>(&mut self, topic: &str, message: &T) -> Result<()> {
        // Clear buffer and encode message
        self.write_buf.clear();
        message.encode(&mut self.write_buf)?;

        // Create Kafka record and send
        let record: BaseRecord<(), [u8], ()> = BaseRecord::to(topic).payload(&self.write_buf);
        self.producer
            .send(record)
            .map_err(|(err, _)| anyhow::anyhow!("Failed to send to topic {}: {}", topic, err))?;

        Ok(())
    }
}

/// Helper to create a ClientQueryProtobuf for reporting receipt data to Kafka
#[allow(clippy::too_many_arguments)]
pub fn create_client_query_report(
    query_id: String,
    receipt_signer: Address,
    allocation_id: Address,
    collection_id: Option<CollectionId>,
    fee_value: u128,
    response_time_ms: u32,
    indexer_url: &str,
    api_key: &str,
) -> ClientQueryProtobuf {
    let fee_grt = fee_value as f64 * 1e-18; // Convert wei to GRT
    let total_fees_usd = fee_grt * 1.0; // Using 1:1 GRT:USD rate for testing

    // Create IndexerQueryProtobuf
    let indexer_query = IndexerQueryProtobuf {
        indexer: allocation_id.as_slice().to_vec(), // Using allocation as indexer ID for simplicity
        deployment: allocation_id.as_slice().to_vec(), // Using allocation as deployment for simplicity
        allocation: if collection_id.is_none() {
            Some(allocation_id.as_slice().to_vec())
        } else {
            None
        },
        collection: collection_id.map(|c| c.0.as_slice().to_vec()),
        indexed_chain: "hardhat".to_string(), // From local network config
        url: indexer_url.to_string(),
        fee_grt,
        response_time_ms,
        seconds_behind: 0,
        result: "success".to_string(),
        indexer_errors: String::new(),
        blocks_behind: 0,
    };

    ClientQueryProtobuf {
        gateway_id: "local".to_string(), // Matching gateway config
        receipt_signer: receipt_signer.as_slice().to_vec(),
        query_id,
        api_key: api_key.to_string(),
        user_id: "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(), // Matching gateway config
        subgraph: None,
        result: "success".to_string(),
        response_time_ms,
        request_bytes: 100,        // Approximate
        response_bytes: Some(200), // Approximate
        total_fees_usd,
        indexer_queries: vec![indexer_query],
    }
}
