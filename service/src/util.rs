use ethers::signers::{
    coins_bip39::English, LocalWallet, MnemonicBuilder, Signer, Wallet, WalletError,
};
use ethers_core::k256::ecdsa::SigningKey;
use lazy_static::lazy_static;
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use tokio::signal;
use toml::Value;
use tracing::{
    info,
    subscriber::{set_global_default, SetGlobalDefaultError},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use crate::common::indexer_error::{indexer_error, IndexerError};

lazy_static! {
    pub static ref DATABASE_URL: String =
        env::var("DATABASE_URL").expect("DATABASE_URL is not set");
}

/// Struct for version control
#[derive(Serialize, Debug, Clone)]
pub struct PackageVersion {
    version: String,
    dependencies: HashMap<String, String>,
}

/// Read the manfiest
fn read_manifest() -> Result<Value, IndexerError> {
    let toml_string = fs::read_to_string("service/Cargo.toml")
        .map_err(|_e| indexer_error(crate::common::indexer_error::IndexerErrorCode::IE074))?;
    let toml_value: Value = toml::from_str(&toml_string)
        .map_err(|_e| indexer_error(crate::common::indexer_error::IndexerErrorCode::IE074))?;
    Ok(toml_value)
}

/// Parse package versioning from the manifest
pub fn package_version() -> Result<PackageVersion, IndexerError> {
    read_manifest().map(|toml_file| {
        let pkg = toml_file.as_table().unwrap();
        let version = pkg
            .get("package")
            .and_then(|p| p.get("version"))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let dependencies = pkg.get("dependencies").and_then(|d| d.as_table()).unwrap();
        let indexer_native = dependencies
            .get("indexer-native")
            .map(|d| d.as_str().unwrap().to_string());

        let release = PackageVersion {
            version,
            dependencies: match indexer_native {
                Some(indexer_native_version) => {
                    let mut map = HashMap::new();
                    map.insert("indexer-native".to_string(), indexer_native_version);
                    map
                }
                None => HashMap::new(),
            },
        };
        info!("Running package version {:#?}", release);

        release
    })
}

pub fn build_wallet(value: &str) -> Result<Wallet<SigningKey>, WalletError> {
    value
        .parse::<LocalWallet>()
        .or(MnemonicBuilder::<English>::default().phrase(value).build())
}

/// Get wallet public address to String
pub fn wallet_address(wallet: &Wallet<SigningKey>) -> String {
    format!("{:?}", wallet.address())
}

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
