// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::{BigDecimal, FromPrimitive, ToPrimitive};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde_repr::Deserialize_repr;
use serde_with::DurationSecondsWithFrac;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};
use tracing::warn;

use alloy_primitives::Address;
use bip39::Mnemonic;
use serde::Deserialize;
use serde_with::serde_as;
use thegraph::types::DeploymentId;
use url::Url;

use crate::NonZeroGRT;

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub indexer: IndexerConfig,
    pub database: DatabaseConfig,
    pub graph_node: GraphNodeConfig,
    pub metrics: MetricsConfig,
    pub subgraphs: SubgraphsConfig,
    pub blockchain: BlockchainConfig,
    pub service: ServiceConfig,
    pub tap: TapConfig,
}

pub enum ConfigPrefix {
    Tap,
    Service,
}

impl ConfigPrefix {
    fn get_prefix(&self) -> &'static str {
        match self {
            Self::Tap => "TAP_AGENT_",
            Self::Service => "INDEXER_SERVICE_",
        }
    }
}

impl Config {
    pub fn parse(prefix: ConfigPrefix, filename: &PathBuf) -> Result<Self, String> {
        let config_defaults = include_str!("../default_values.toml");

        let config: Self = Figment::new()
            .merge(Toml::string(config_defaults))
            .merge(Toml::file(filename))
            .merge(Env::prefixed(prefix.get_prefix()))
            .extract()
            .map_err(|e| e.to_string())?;
        config.validate()?;

        Ok(config)
    }

    // custom validation of the values
    fn validate(&self) -> Result<(), String> {
        match &self.tap.rav_request.trigger_value_divisor {
            x if *x <= 1.into() => {
                return Err("trigger_value_divisor must be greater than 1".to_string())
            }
            x if *x > 1.into() && *x < 10.into() => warn!(
                "It's recommended that trigger_value_divisor \
                be a value greater than 10."
            ),
            _ => {}
        }

        let ten: BigDecimal = 10.into();
        let usual_grt_price = BigDecimal::from_str("0.0001").unwrap() * ten;
        if self.tap.max_amount_willing_to_lose_grt.get_value() < usual_grt_price.to_u128().unwrap()
        {
            warn!(
                "Your `max_amount_willing_to_lose_grt` value is too close to zero. \
                This may deny the sender too often or even break the whole system. \
                It's recommended it to be a value greater than 100x an usual query price."
            );
        }

        if self.subgraphs.escrow.config.syncing_interval_secs < Duration::from_secs(10)
            || self.subgraphs.network.config.syncing_interval_secs < Duration::from_secs(10)
        {
            warn!(
                "Your `syncing_interval_secs` value it too low. \
                This may overload your graph-node instance, \
                a recommended value is about 60 seconds."
            );
        }

        if self.subgraphs.escrow.config.syncing_interval_secs > Duration::from_secs(600)
            || self.subgraphs.network.config.syncing_interval_secs > Duration::from_secs(600)
        {
            warn!(
                "Your `syncing_interval_secs` value it too high. \
                This may cause issues while reacting to updates in the blockchain. \
                a recommended value is about 60 seconds."
            );
        }

        if self.tap.rav_request.timestamp_buffer_secs < Duration::from_secs(10) {
            warn!(
                "Your `tap.rav_request.timestamp_buffer_secs` value it too low. \
                You may discart receipts in case of any synchronization issues, \
                a recommended value is about 30 seconds."
            );
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct IndexerConfig {
    pub indexer_address: Address,
    pub operator_mnemonic: Mnemonic,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    pub postgres_url: Url,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct GraphNodeConfig {
    pub query_url: Url,
    pub status_url: Url,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    pub port: u16,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct SubgraphsConfig {
    pub network: NetworkSubgraphConfig,
    pub escrow: EscrowSubgraphConfig,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct NetworkSubgraphConfig {
    #[serde(flatten)]
    pub config: SubgraphConfig,

    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub recently_closed_allocation_buffer_secs: Duration,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct EscrowSubgraphConfig {
    #[serde(flatten)]
    pub config: SubgraphConfig,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct SubgraphConfig {
    pub query_url: Url,
    pub query_auth_token: Option<String>,
    pub deployment_id: Option<DeploymentId>,
    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub syncing_interval_secs: Duration,
}

#[derive(Debug, Deserialize_repr, Clone)]
#[cfg_attr(test, derive(PartialEq))]
#[repr(u64)]
pub enum TheGraphChainId {
    Ethereum = 1,
    Goerli = 5,
    Sepolia = 11155111,
    Arbitrum = 42161,
    ArbitrumGoerli = 421613,
    ArbitrumSepolia = 421614,
    Test = 1337,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct BlockchainConfig {
    pub chain_id: TheGraphChainId,
    pub receipts_verifier_address: Address,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub serve_network_subgraph: bool,
    pub serve_escrow_subgraph: bool,
    pub serve_auth_token: Option<String>,
    pub host_and_port: SocketAddr,
    pub url_prefix: String,
    pub tap: ServiceTapConfig,
    pub free_query_auth_token: Option<String>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct ServiceTapConfig {
    /// what's the maximum value we accept in a receipt
    pub max_receipt_value_grt: NonZeroGRT,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct TapConfig {
    /// what is the maximum amount the indexer is willing to lose in grt
    pub max_amount_willing_to_lose_grt: NonZeroGRT,
    pub rav_request: RavRequestConfig,

    pub sender_aggregator_endpoints: HashMap<Address, Url>,
}

impl TapConfig {
    pub fn get_trigger_value(&self) -> u128 {
        let grt_wei = self.max_amount_willing_to_lose_grt.get_value();
        let decimal = BigDecimal::from_u128(grt_wei).unwrap();
        let divisor = &self.rav_request.trigger_value_divisor;
        (decimal / divisor)
            .to_u128()
            .expect("Could not represent the trigger value in u128")
    }
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields)]
pub struct RavRequestConfig {
    /// what divisor of the amount willing to lose to trigger the rav request
    pub trigger_value_divisor: BigDecimal,
    /// timestamp buffer
    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub timestamp_buffer_secs: Duration,
    /// timeout duration while requesting a rav
    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub request_timeout_secs: Duration,
    /// how many receipts are sent in a single rav requests
    pub max_receipts_per_request: u64,
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::{Config, ConfigPrefix};

    #[test]
    fn test_minimal_config() {
        Config::parse(
            ConfigPrefix::Service,
            &PathBuf::from("minimal-config-example.toml"),
        )
        .unwrap();
    }

    #[test]
    fn test_maximal_config() {
        // Generate full config by deserializing the minimal config and let the code fill in the defaults.
        let max_config = Config::parse(
            ConfigPrefix::Service,
            &PathBuf::from("minimal-config-example.toml"),
        )
        .unwrap();
        let max_config_file: Config = toml::from_str(
            fs::read_to_string("maximal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();

        assert_eq!(max_config, max_config_file);
    }
}
