// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc};

use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};
use tokio::{sync::Semaphore, task, time::Instant};

use crate::{
    constants::{
        ACCOUNT0_SECRET, CHAIN_ID, GRAPH_URL, INDEXER_URL, MAX_RECEIPT_VALUE, SUBGRAPH_ID,
        TAP_VERIFIER_CONTRACT,
    },
    utils::{create_request, create_tap_receipt, create_tap_receipt_v2, find_allocation},
};

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

// V2 version of the load test using Horizon (V2) receipts with collection_id
pub async fn receipt_handler_load_test_v2(num_receipts: usize, concurrency: usize) -> Result<()> {
    let wallet: PrivateKeySigner = ACCOUNT0_SECRET.parse().unwrap();

    // Setup HTTP client
    let http_client = Arc::new(Client::new());

    // Query the network subgraph to find active allocations
    let allocation_id = find_allocation(http_client.clone(), GRAPH_URL).await?;
    let allocation_id = Address::from_str(&allocation_id)?;

    // For V2, we need payer and service provider addresses
    let payer = wallet.address();
    let service_provider = allocation_id; // Using allocation_id as service provider for simplicity

    let start = Instant::now();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = vec![];

    for _ in 0..num_receipts {
        let signer = wallet.clone();
        let client = http_client.clone();

        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let handle = task::spawn(async move {
            let res =
                create_and_send_receipts_v2(allocation_id, signer, client, payer, service_provider)
                    .await;
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
                    eprintln!("V2 Receipt {} failed to send: {:?}", index, e); // Log the specific error
                } else {
                    successful_sends += 1;
                }
            }
            Err(join_error) => {
                // The task panicked or was cancelled
                failed_sends += 1;
                eprintln!(
                    "V2 Receipt {} task execution failed (e.g., panic): {:?}",
                    index, join_error
                );
            }
        }
    }

    let duration = start.elapsed();
    println!(
        "Completed processing {} V2 requests in {:?}",
        num_receipts, duration
    );
    if num_receipts > 0 {
        println!(
            "Average time per V2 request: {:?}",
            duration / num_receipts as u32
        );
    }
    println!("Successfully sent V2 receipts: {}", successful_sends);
    println!("Failed V2 receipts: {}", failed_sends);

    if failed_sends > 0 {
        return Err(anyhow::anyhow!(
            "V2 Load test completed with {} failures.",
            failed_sends
        ));
    }

    Ok(())
}

async fn create_and_send_receipts_v2(
    allocation_id: Address,
    signer: PrivateKeySigner,
    http_client: Arc<Client>,
    payer: Address,
    service_provider: Address,
) -> Result<()> {
    let receipt = create_tap_receipt_v2(
        MAX_RECEIPT_VALUE,
        &allocation_id,
        TAP_VERIFIER_CONTRACT,
        CHAIN_ID,
        &signer,
        &payer,
        &service_provider,
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
            "Failed to send V2 receipt: {}",
            response.text().await.unwrap_or_default()
        ));
    }

    Ok(())
}
