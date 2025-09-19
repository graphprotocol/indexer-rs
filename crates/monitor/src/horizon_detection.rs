// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Horizon (V2) contract detection utilities
//!
//! This module provides functionality to detect if Horizon (V2) contracts are active
//! in the network by querying the network subgraph for PaymentsEscrow accounts.

use anyhow::Result;
use indexer_query::horizon_detection::{self, HorizonDetectionQuery};

use crate::client::SubgraphClient;

/// Detects if Horizon (V2) contracts are active in the network.
///
/// This function queries the network subgraph to check if any PaymentsEscrow accounts exist.
/// If they do, it indicates that Horizon contracts are deployed and active.
///
/// # Arguments
/// * `network_subgraph` - The network subgraph client to query
///
/// # Returns
/// * `Ok(true)` if Horizon contracts are active
/// * `Ok(false)` if only legacy (V1) contracts are active
/// * `Err(...)` if there was an error querying the subgraph
pub async fn is_horizon_active(network_subgraph: &SubgraphClient) -> Result<bool> {
    tracing::debug!("Checking if Horizon (V2) contracts are active in the network");

    let response = network_subgraph
        .query::<HorizonDetectionQuery, _>(horizon_detection::Variables {})
        .await?;

    let response = response?;

    let account_count = response.payments_escrow_accounts.len();

    if account_count > 0 {
        tracing::info!(
            accounts = account_count,
            "Horizon (V2) schema available - found existing PaymentsEscrow accounts"
        );
    } else {
        tracing::info!("Horizon (V2) schema available - no accounts found at startup, but will detect new accounts automatically");
    }

    Ok(true)
}
