// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use ethers::signers::WalletError;
use tokio::signal;
use tracing::{
    info,
    subscriber::{set_global_default, SetGlobalDefaultError},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use crate::common::address::{build_wallet, wallet_address};

/// Validate that private key as an Eth wallet
pub fn public_key(value: &str) -> Result<String, WalletError> {
    // The wallet can be stored instead of the original private key
    let wallet = build_wallet(value)?;
    let addr = wallet_address(&wallet);
    info!(address = addr, "Resolved Graphcast id");
    Ok(addr)
}

/// Sets up tracing, allows log level to be set from the environment variables
pub fn init_tracing(format: String) -> Result<(), SetGlobalDefaultError> {
    let filter = EnvFilter::from_default_env();

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

pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("signal received, starting graceful shutdown");
}
