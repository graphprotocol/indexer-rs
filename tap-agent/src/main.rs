// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info};

use indexer_tap_agent::{agent, CONFIG};

#[tokio::main]
async fn main() -> Result<()> {
    // Parse basic configurations, also initializes logging.
    lazy_static::initialize(&CONFIG);
    debug!("Config: {:?}", *CONFIG);

    let manager = agent::start_agent().await;
    info!("TAP Agent started.");

    // Have tokio wait for SIGTERM or SIGINT.
    let mut signal_sigint = signal(SignalKind::interrupt())?;
    let mut signal_sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = signal_sigint.recv() => debug!("Received SIGINT."),
        _ = signal_sigterm.recv() => debug!("Received SIGTERM."),
    }
    // If we're here, we've received a signal to exit.
    info!("Shutting down...");

    // We don't want our actor to run any shutdown logic, so we kill it.
    manager
        .kill_and_wait(None)
        .await
        .expect("Failed to kill manager.");

    // Stop the server and wait for it to finish gracefully.
    debug!("Goodbye!");
    Ok(())
}
