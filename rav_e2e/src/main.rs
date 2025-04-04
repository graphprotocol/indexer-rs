// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

mod metrics;
mod receipt;
mod rav_tests;

use rav_tests::test_rav_generation;
use receipt::create_tap_receipt;


#[tokio::main]
async fn main() -> Result<()> {

    // Run the TAP receipt test
    test_rav_generation().await
}
