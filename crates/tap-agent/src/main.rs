// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

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

    // 🚀 PRODUCTION IMPLEMENTATION: Stream-based TAP agent (TAP_AGENT_TOKIO_DESIGN.md)
    tokio::spawn(metrics::run_server(CONFIG.metrics.port));
    tracing::info!("Metrics port opened");

    // Run the stream-based TAP agent directly - our production implementation
    let agent_task = tokio::spawn(agent::start_stream_based_agent());

    tracing::info!("🚀 Stream-based TAP Agent started (production implementation)");

    // Have tokio wait for SIGTERM or SIGINT.
    let mut signal_sigint = signal(SignalKind::interrupt())?;
    let mut signal_sigterm = signal(SignalKind::terminate())?;

    tokio::select! {
        result = agent_task => {
            match result {
                Ok(Ok(())) => tracing::info!("TAP Agent completed successfully"),
                Ok(Err(e)) => tracing::error!(error = %e, "TAP Agent failed"),
                Err(e) => tracing::error!(error = %e, "TAP Agent task panicked"),
            }
        }
        _ = signal_sigint.recv() => tracing::info!("Received SIGINT - initiating graceful shutdown"),
        _ = signal_sigterm.recv() => tracing::info!("Received SIGTERM - initiating graceful shutdown"),
    }

    // If we're here, we've received a signal to exit or the agent completed
    tracing::info!("TAP Agent shutting down gracefully...");

    // The stream-based agent handles its own graceful shutdown via channel closure semantics
    // Give it a moment to complete any in-flight work
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    tracing::info!("✅ TAP Agent shutdown complete - goodbye!");
    Ok(())
}
