// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

mod constants;
mod load_test;
mod metrics;
mod rav_tests;
mod utils;

use anyhow::Result;
use clap::Parser;
use load_test::receipt_handler_load_test;
use metrics::MetricsChecker;
pub(crate) use rav_tests::{test_invalid_chain_id, test_tap_rav_v1};

/// Main CLI parser structure
#[derive(Parser, Debug)]
#[clap(author, version, about = "Integration Test Suite Runner", long_about = None)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Rav1,

    #[clap(name = "load")]
    LoadService {
        // for example: --num-receipts 10000 or -n 10000
        #[clap(long, short, value_parser)]
        num_receipts: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // cargo run -- rav1
        Commands::Rav1 => {
            test_invalid_chain_id().await?;
            test_tap_rav_v1().await?;
        }
        // cargo run -- load --num-receipts 1000
        Commands::LoadService { num_receipts } => {
            let concurrency = num_cpus::get();
            receipt_handler_load_test(num_receipts, concurrency).await?;
        }
    }

    Ok(())
}
