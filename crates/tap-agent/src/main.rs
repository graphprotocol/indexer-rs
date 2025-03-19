// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_tap_agent::{agent, metrics, CONFIG};
use ractor::ActorStatus;
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse basic configurations, also initializes logging.

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

    // We don't want our actor to run any shutdown logic, so we kill it.
    if manager.get_status() == ActorStatus::Running {
        manager
            .kill_and_wait(None)
            .await
            .expect("Failed to kill manager.");
    }

    // Stop the server and wait for it to finish gracefully.
    tracing::debug!("Goodbye!");
    Ok(())
}
