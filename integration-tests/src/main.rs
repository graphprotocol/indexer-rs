// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use rav_tests::{test_invalid_chain_id, test_tap_rav_v1};

mod metrics;
mod rav_tests;
mod utils;

use metrics::MetricsChecker;

#[tokio::main]
async fn main() -> Result<()> {
    // Run the TAP receipt test
    test_invalid_chain_id().await?;
    test_tap_rav_v1().await
}
