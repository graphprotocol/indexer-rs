// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use lazy_static::lazy_static;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info};

use crate::config::Cli;

mod agent;
mod aggregator_endpoints;
mod config;
mod database;
mod tap;

lazy_static! {
    pub static ref CONFIG: Cli = Cli::args();
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse basic configurations, also initializes logging.
    lazy_static::initialize(&CONFIG);
    debug!("Config: {:?}", *CONFIG);

    {
        let _manager = agent::start_agent(&CONFIG).await;
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
    }
    // Manager should be successfully dropped here.

    // Stop the server and wait for it to finish gracefully.
    debug!("Goodbye!");
    Ok(())
}
