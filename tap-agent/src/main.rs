// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use ractor::ActorStatus;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, error, info};

use indexer_tap_agent::{agent, metrics, CONFIG};

#[tokio::main]
async fn main() -> Result<()> {
    // Parse basic configurations, also initializes logging.
    lazy_static::initialize(&CONFIG);

    let (manager, handler) = agent::start_agent().await;
    info!("TAP Agent started.");

    tokio::spawn(metrics::run_server(
        CONFIG.indexer_infrastructure.metrics_port,
    ));
    info!("Metrics port opened");

    // Have tokio wait for SIGTERM or SIGINT.
    let mut signal_sigint = signal(SignalKind::interrupt())?;
    let mut signal_sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = handler => error!("SenderAccountsManager stopped"),
        _ = signal_sigint.recv() => debug!("Received SIGINT."),
        _ = signal_sigterm.recv() => debug!("Received SIGTERM."),
    }
    // If we're here, we've received a signal to exit.
    info!("Shutting down...");

    // We don't want our actor to run any shutdown logic, so we kill it.
    if manager.get_status() == ActorStatus::Running {
        manager
            .kill_and_wait(None)
            .await
            .expect("Failed to kill manager.");
    }

    // Stop the server and wait for it to finish gracefully.
    debug!("Goodbye!");
    Ok(())
}
