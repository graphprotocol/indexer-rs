// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use clap::Parser;
use indexer_config::{Config as IndexerConfig, ConfigPrefix};
use tracing::{
    level_filters::LevelFilter,
    subscriber::{set_global_default, SetGlobalDefaultError},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser)]
#[command(version)]
pub struct Cli {
    /// Path to the configuration file.
    /// See https://github.com/graphprotocol/indexer-rs/tree/main/tap-agent for examples.
    #[arg(long, value_name = "FILE", verbatim_doc_comment)]
    pub config: Option<PathBuf>,
}

/// Sets up tracing, allows log level to be set from the environment variables
fn init_tracing(format: String) -> Result<(), SetGlobalDefaultError> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let subscriber_builder: tracing_subscriber::fmt::SubscriberBuilder<
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format,
        EnvFilter,
    > = FmtSubscriber::builder().with_env_filter(filter);
    match format.as_str() {
        "json" => set_global_default(subscriber_builder.json().finish()),
        "full" => set_global_default(subscriber_builder.finish()),
        "compact" => set_global_default(subscriber_builder.compact().finish()),
        _ => set_global_default(subscriber_builder.with_ansi(true).pretty().finish()),
    }
}

pub fn get_config() -> anyhow::Result<IndexerConfig> {
    let cli = Cli::parse();
    let config = IndexerConfig::parse(ConfigPrefix::Tap, cli.config.as_ref()).map_err(|e| {
        tracing::error!(
            "Invalid configuration file `{}`: {}, if a value is missing you can also use \
                --config to fill the rest of the values",
            cli.config.unwrap_or_default().display(),
            e
        );
        anyhow::anyhow!(e)
    })?;

    // add a LogFormat to config
    init_tracing("pretty".to_string()).expect(
        "Could not set up global default subscriber for logger, check \
        environmental variable `RUST_LOG`",
    );

    Ok(config)
}
