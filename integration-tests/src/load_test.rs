// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use thegraph_core::alloy::primitives::Address;
use thegraph_core::alloy::signers::local::PrivateKeySigner;
use tokio::sync::Semaphore;
use tokio::task;
use tokio::time::Instant;

use crate::utils::{create_request, create_tap_receipt, find_allocation};

const INDEXER_URL: &str = "http://localhost:7601";
// Taken from .env
// this is the key gateway uses
const ACCOUNT0_SECRET: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

// The deployed gateway and indexer
// use this verifier contract
// which must be part of the eip712 domain
const TAP_VERIFIER_CONTRACT: &str = "0x8198f5d8F8CfFE8f9C413d98a0A55aEB8ab9FbB7";
const CHAIN_ID: u64 = 1337;

const SUBGRAPH_ID: &str = "QmV4R5g7Go94bVFmKTVFG7vaMTb1ztUUWb45mNrsc7Yyqs";

const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

const GRT_DECIMALS: u8 = 18;
const GRT_BASE: u128 = 10u128.pow(GRT_DECIMALS as u32);

const MAX_RECEIPT_VALUE: u128 = GRT_BASE / 10_000;

// Function to test indexer service component
// which is in charge of validating receipt signature,
// amount, timestamp and so on,  and store them into the database.
// it is the entry point for the TAP receipts
// processing into RAVs(the slower part)
pub async fn receipt_handler_load_test(num_receipts: usize, concurrency: usize) -> Result<()> {
    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse().unwrap();

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;
    let allocation_id = Address::from_str(&allocation_id)?;

    let start = Instant::now();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = vec![];

    for _ in 0..num_receipts {
        let signer = wallet.clone();
        let client = http_client.clone();

        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let handle = task::spawn(async move {
            let res = create_and_send_receipts(allocation_id, signer, client).await;
            drop(permit);
            res
        });
        handles.push(handle);
    }

    let mut successful_sends = 0;
    let mut failed_sends = 0;

    for (index, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(send_result) => {
                // Check if the send was Ok
                if let Err(e) = send_result {
                    failed_sends += 1;
                    eprintln!("Receipt {} failed to send: {:?}", index, e); // Log the specific error
                } else {
                    successful_sends += 1;
                }
            }
            Err(join_error) => {
                // The task panicked or was cancelled
                failed_sends += 1;
                eprintln!(
                    "Receipt {} task execution failed (e.g., panic): {:?}",
                    index, join_error
                );
            }
        }
    }

    let duration = start.elapsed();
    println!(
        "Completed processing {} requests in {:?}",
        num_receipts, duration
    );
    if num_receipts > 0 {
        println!(
            "Average time per request: {:?}",
            duration / num_receipts as u32
        );
    }
    println!("Successfully sent receipts: {}", successful_sends);
    println!("Failed receipts: {}", failed_sends);

    if failed_sends > 0 {
        return Err(anyhow::anyhow!(
            "Load test completed with {} failures.",
            failed_sends
        ));
    }

    Ok(())
}

async fn create_and_send_receipts(
    id: Address,
    signer: PrivateKeySigner,
    http_client: Arc<Client>,
) -> Result<()> {
    let receipt = create_tap_receipt(
        MAX_RECEIPT_VALUE,
        &id,
        TAP_VERIFIER_CONTRACT,
        CHAIN_ID,
        &signer,
    )?;

    let receipt_json = serde_json::to_string(&receipt).unwrap();
    let response = create_request(
        &http_client,
        format!("{}/subgraphs/id/{}", INDEXER_URL, SUBGRAPH_ID).as_str(),
        &receipt_json,
        &json!({
            "query": "{ _meta { block { number } } }"
        }),
    )
    .send()
    .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to send receipt: {}",
            response.text().await.unwrap_or_default()
        ));
    }

    Ok(())
}
