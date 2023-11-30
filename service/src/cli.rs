// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Cli {
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
}
