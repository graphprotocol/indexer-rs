use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Cli {
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
}
