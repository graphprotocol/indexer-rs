// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_tap_agent::actor_migrate::TaskStatus;
use indexer_tap_agent::{agent, metrics, CONFIG};
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(all(feature = "profiling", not(test)))]
    if let Err(e) = profiler::setup_profiling(
        "/opt/profiling/tap-agent".to_string(),
        150,
        120,
        Some("Tap-agent service".to_string()),
    ) {
        // If profiling fails, log the error
        // but continue running the application
        // as profiling is just for development.
        tracing::error!("Failed to setup profiling: {e}");
    } else {
        tracing::info!("Profiling setup complete.");
    }

    // initialize LazyLock'd config
    _ = &*CONFIG;

    let (manager, handler) = agent::start_agent().await;
    tracing::info!("TAP Agent started.");

    tokio::spawn(metrics::run_server(CONFIG.metrics.port));
    tracing::info!("Metrics port opened");

    // Have tokio wait for SIGTERM or SIGINT.
    let mut signal_sigint = signal(SignalKind::interrupt())?;
    let mut signal_sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = handler => tracing::error!("SenderAccountsManager stopped"),
        _ = signal_sigint.recv() => tracing::debug!("Received SIGINT."),
        _ = signal_sigterm.recv() => tracing::debug!("Received SIGTERM."),
    }
    // If we're here, we've received a signal to exit.
    tracing::info!("Shutting down...");

    // 🎯 TOKIO MIGRATION: Use TaskHandle methods instead of ActorRef methods
    if manager.get_status().await == TaskStatus::Running {
        tracing::info!("Stopping sender accounts manager task...");
        manager.stop(Some("Shutdown signal received".to_string()));

        // Give the task a moment to shut down gracefully
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Stop the server and wait for it to finish gracefully.
    tracing::debug!("Goodbye!");
    Ok(())
}
