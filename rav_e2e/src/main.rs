// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use rav_tests::tap_rav_test;

mod metrics;
mod rav_tests;
mod receipt;
use metrics::MetricsChecker;
use receipt::create_tap_receipt;

#[tokio::main]
async fn main() -> Result<()> {
    // Run the TAP receipt test
    tap_rav_test().await
}
