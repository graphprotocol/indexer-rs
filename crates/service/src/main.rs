// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::process::ExitCode;

use indexer_service_rs::service::run;
use tracing::{level_filters::LevelFilter, subscriber::set_global_default};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    #[cfg(all(feature = "profiling", not(test)))]
    if let Err(e) = profiler::setup_profiling(
        "/opt/profiling/indexer-service".to_string(),
        150,
        120,
        Some("Indexer Service".to_string()),
    ) {
        // If profiling fails, log the error
        // but continue running the application
        // as profiling is just for development.
        tracing::error!("Failed to setup profiling: {e}");
    } else {
        tracing::info!("Profiling setup complete.");
    }

    if let Err(e) = run().await {
        tracing::error!("Indexer service error: {e}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn init_tracing() {
    // Tracing setup
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let subscriber_builder: tracing_subscriber::fmt::SubscriberBuilder<
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format,
        EnvFilter,
    > = FmtSubscriber::builder().with_env_filter(filter);
    set_global_default(subscriber_builder.with_ansi(true).pretty().finish()).expect(
        "Could not set up global default subscriber for logger, check \
        environmental variable `RUST_LOG`",
    );
}
