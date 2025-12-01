// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use bigdecimal::{BigDecimal, FromPrimitive, ToPrimitive};
use bip39::Mnemonic;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use regex::Regex;
use serde::Deserialize;
use serde_repr::Deserialize_repr;
use serde_with::{serde_as, DurationSecondsWithFrac};
use thegraph_core::{
    alloy::primitives::{Address, U256},
    DeploymentId,
};
use url::Url;

use crate::NonZeroGRT;

const SHARED_PREFIX: &str = "INDEXER_";

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Config {
    pub indexer: IndexerConfig,
    pub database: DatabaseConfig,
    pub graph_node: GraphNodeConfig,
    pub metrics: MetricsConfig,
    pub subgraphs: SubgraphsConfig,
    pub blockchain: BlockchainConfig,
    pub service: ServiceConfig,
    pub tap: TapConfig,
    pub dips: Option<DipsConfig>,
    pub horizon: HorizonConfig,
}

// Newtype wrapping Config to be able use serde_ignored with Figment
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ConfigWrapper(pub Config);

// Custom Deserializer for ConfigWrapper
// This is needed to warn about unknown fields
impl<'de> Deserialize<'de> for ConfigWrapper {
    fn deserialize<D>(deserializer: D) -> Result<ConfigWrapper, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let config: Config = serde_ignored::deserialize(deserializer, |path| {
            tracing::warn!("Ignoring unknown configuration field: {}", path);
        })?;

        Ok(ConfigWrapper(config))
    }
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
    pub fn parse(prefix: ConfigPrefix, filename: Option<&PathBuf>) -> Result<Self, String> {
        let config_defaults = include_str!("../default_values.toml");

        let mut figment_config = Figment::new().merge(Toml::string(config_defaults));

        if let Some(path) = filename {
            let mut config_content = std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read config file: {e}"))?;
            config_content = Self::substitute_env_vars(config_content)?;
            figment_config = figment_config.merge(Toml::string(&config_content));
        }

        let config: ConfigWrapper = figment_config
            .merge(Self::from_env_ignore_empty(prefix.get_prefix()))
            .merge(Self::from_env_ignore_empty(SHARED_PREFIX))
            .extract()
            .map_err(|e| e.to_string())?;

        config.0.validate()?;
        Ok(config.0)
    }

    fn from_env_ignore_empty(prefix: &str) -> Env {
        let prefixed_env = Env::prefixed(prefix).split("__");
        let ignore_prefixed: Vec<_> = prefixed_env
            .iter()
            .filter_map(|(key, value)| {
                if value.is_empty() {
                    Some(key.into_string())
                } else {
                    None
                }
            })
            .collect();
        let ref_ignore = ignore_prefixed
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>();
        prefixed_env.ignore(&ref_ignore)
    }

    fn substitute_env_vars(content: String) -> Result<String, String> {
        let reg = Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").map_err(|e| e.to_string())?;
        let mut missing_vars = Vec::new();
        let mut result = String::new();

        for line in content.lines() {
            if !line.trim_start().starts_with('#') {
                let processed_line = reg.replace_all(line, |caps: &regex::Captures| {
                    let var_name = &caps[1];
                    match env::var(var_name) {
                        Ok(value) => value,
                        Err(_) => {
                            missing_vars.push(var_name.to_string());
                            format!("${{{var_name}}}")
                        }
                    }
                });
                result.push_str(&processed_line);
                result.push('\n');
            }
        }

        if !missing_vars.is_empty() {
            return Err(format!(
                "Missing environment variables: {}",
                missing_vars.join(", ")
            ));
        }

        Ok(result.trim_end().to_string())
    }

    // custom validation of the values
    fn validate(&self) -> Result<(), String> {
        match &self.tap.rav_request.trigger_value_divisor {
            x if *x <= 1.into() => {
                return Err("trigger_value_divisor must be greater than 1".to_string())
            }
            x if *x > 1.into() && *x < 10.into() => tracing::warn!(
                "It's recommended that trigger_value_divisor \
                be a value greater than 10."
            ),
            _ => {}
        }
        let grt_wei = self.tap.max_amount_willing_to_lose_grt.get_value();
        let decimal = BigDecimal::from_u128(grt_wei).unwrap();
        let divisor = &self.tap.rav_request.trigger_value_divisor;
        let trigger_value = (decimal / divisor)
            .to_u128()
            .expect("Could not represent the trigger value in u128");
        let minimum_recommended_for_max_willing_to_lose_grt = 0.1;
        if trigger_value
            < minimum_recommended_for_max_willing_to_lose_grt
                .to_u128()
                .unwrap()
        {
            tracing::warn!(
                "Trigger value is too low, currently below 0.1 GRT. \
                Please modify `max_amount_willing_to_lose_grt` or `trigger_value_divisor`. \
                It is best to have a higher trigger value, ideally above 1 GRT. \
                Anything lower and the system may constantly deny the sender. \
                `Trigger value`  is defined by: \
                (max_amount_willing_to_lose_grt / trigger_value_divisor) "
            )
        }

        let ten: BigDecimal = 10.into();
        let usual_grt_price = BigDecimal::from_str("0.0001").unwrap() * ten;
        if self.tap.max_amount_willing_to_lose_grt.get_value() < usual_grt_price.to_u128().unwrap()
        {
            tracing::warn!(
                "Your `max_amount_willing_to_lose_grt` value is too close to zero. \
                This may deny the sender too often or even break the whole system. \
                It's recommended it to be a value greater than 100x an usual query price."
            );
        }

        if self.subgraphs.escrow.config.syncing_interval_secs < Duration::from_secs(10)
            || self.subgraphs.network.config.syncing_interval_secs < Duration::from_secs(10)
        {
            tracing::warn!(
                "Your `syncing_interval_secs` value it too low. \
                This may overload your graph-node instance, \
                a recommended value is about 60 seconds."
            );
        }

        if self.subgraphs.escrow.config.syncing_interval_secs > Duration::from_secs(600)
            || self.subgraphs.network.config.syncing_interval_secs > Duration::from_secs(600)
        {
            tracing::warn!(
                "Your `syncing_interval_secs` value it too high. \
                This may cause issues while reacting to updates in the blockchain. \
                a recommended value is about 60 seconds."
            );
        }

        if self.tap.rav_request.timestamp_buffer_secs < Duration::from_secs(10) {
            tracing::warn!(
                "Your `tap.rav_request.timestamp_buffer_secs` value it too low. \
                You may discart receipts in case of any synchronization issues, \
                a recommended value is about 30 seconds."
            );
        }

        // Horizon configuration validation
        // Explicit toggle via `horizon.enabled`. When enabled, require both
        // `blockchain.subgraph_service_address` and
        // `blockchain.receipts_verifier_address_v2` to be present.
        // When disabled, V2 addresses are ignored.
        if self.horizon.enabled {
            if self.blockchain.subgraph_service_address.is_none() {
                return Err(
                    "When horizon.enabled = true, `blockchain.subgraph_service_address` must be set"
                        .to_string(),
                );
            }
            if self.blockchain.receipts_verifier_address_v2.is_none() {
                return Err(
                    "When horizon.enabled = true, `blockchain.receipts_verifier_address_v2` must be set"
                        .to_string(),
                );
            }
        }

        Ok(())
    }

    /// Derive TAP operation mode from horizon configuration
    ///
    /// This method translates the `[horizon]` configuration section into a
    /// [`TapMode`] enum for use throughout the indexer codebase.
    ///
    /// # Returns
    ///
    /// - [`TapMode::Legacy`] if `horizon.enabled = false`
    /// - [`TapMode::Horizon`] if `horizon.enabled = true` with the configured
    ///   `blockchain.subgraph_service_address`
    pub fn tap_mode(&self) -> TapMode {
        if self.horizon.enabled {
            TapMode::Horizon {
                subgraph_service_address: self.blockchain.subgraph_service_address.expect(
                    "subgraph_service_address should be validated during Config::validate()",
                ),
            }
        } else {
            TapMode::Legacy
        }
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct IndexerConfig {
    pub indexer_address: Address,
    pub operator_mnemonic: Mnemonic,
}

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(untagged)]
#[serde(deny_unknown_fields)]
pub enum DatabaseConfig {
    PostgresUrl {
        postgres_url: Url,
    },
    PostgresVars {
        host: String,
        port: Option<u16>,
        user: String,
        password: Option<String>,
        database: String,
    },
}
impl DatabaseConfig {
    pub fn get_formated_postgres_url(self) -> Url {
        match self {
            DatabaseConfig::PostgresUrl { postgres_url } => postgres_url,
            DatabaseConfig::PostgresVars {
                host,
                port,
                user,
                password,
                database,
            } => {
                let postgres_url_str = format!("postgres://{user}@{host}/{database}");
                let mut postgres_url =
                    Url::parse(&postgres_url_str).expect("Failed to parse database_url");
                postgres_url
                    .set_password(password.as_deref())
                    .expect("Failed to set password for database");
                postgres_url
                    .set_port(port)
                    .expect("Failed to set port for database");
                postgres_url
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct GraphNodeConfig {
    pub query_url: Url,
    pub status_url: Url,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct MetricsConfig {
    pub port: u16,
}

impl MetricsConfig {
    pub fn get_socket_addr(&self) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), self.port))
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct SubgraphsConfig {
    pub network: NetworkSubgraphConfig,
    pub escrow: EscrowSubgraphConfig,
    // Note: V2 escrow accounts are in the network subgraph, not a separate escrow_v2 subgraph
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct NetworkSubgraphConfig {
    #[serde(flatten)]
    pub config: SubgraphConfig,

    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub recently_closed_allocation_buffer_secs: Duration,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct EscrowSubgraphConfig {
    #[serde(flatten)]
    pub config: SubgraphConfig,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct SubgraphConfig {
    pub query_url: Url,
    pub query_auth_token: Option<String>,
    pub deployment_id: Option<DeploymentId>,
    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub syncing_interval_secs: Duration,
}

#[derive(Debug, Deserialize_repr, Clone, Copy)]
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
pub struct BlockchainConfig {
    pub chain_id: TheGraphChainId,
    pub receipts_verifier_address: Address,
    /// Verifier address for V2 receipts(Horizon)
    /// after transition period this will be the only address used
    /// to verify receipts
    pub receipts_verifier_address_v2: Option<Address>,
    /// Address of the SubgraphService contract used for V2 operations
    pub subgraph_service_address: Option<Address>,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ServiceConfig {
    pub ipfs_url: Url,
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
pub struct ServiceTapConfig {
    /// what's the maximum value we accept in a receipt
    pub max_receipt_value_grt: NonZeroGRT,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct TapConfig {
    /// what is the maximum amount the indexer is willing to lose in grt
    pub max_amount_willing_to_lose_grt: NonZeroGRT,
    pub rav_request: RavRequestConfig,

    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub sender_timeout_secs: Duration,

    pub sender_aggregator_endpoints: HashMap<Address, Url>,

    /// Senders that are allowed to spend up to `max_amount_willing_to_lose_grt`
    /// over the escrow balance
    #[serde(default)]
    pub trusted_senders: HashSet<Address>,

    /// Interval in seconds for periodic allocation reconciliation.
    /// This ensures stale allocations are detected after subgraph connectivity issues.
    /// Default: 300 (5 minutes)
    #[serde(default = "default_allocation_reconciliation_interval_secs")]
    #[serde_as(as = "DurationSecondsWithFrac<f64>")]
    pub allocation_reconciliation_interval_secs: Duration,
}

fn default_allocation_reconciliation_interval_secs() -> Duration {
    Duration::from_secs(300)
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct DipsConfig {
    pub host: String,
    pub port: String,
    pub allowed_payers: Vec<Address>,

    pub price_per_entity: U256,
    pub price_per_epoch: BTreeMap<String, U256>,
    pub additional_networks: HashMap<String, String>,
}

impl Default for DipsConfig {
    fn default() -> Self {
        DipsConfig {
            host: "0.0.0.0".to_string(),
            port: "7601".to_string(),
            allowed_payers: vec![],
            price_per_entity: U256::from(100),
            price_per_epoch: BTreeMap::new(),
            additional_networks: HashMap::new(),
        }
    }
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

/// TAP protocol operation mode
///
/// Defines whether the indexer operates in legacy mode (V1 TAP receipts only)
/// or horizon mode (hybrid V1/V2 TAP receipts support).
///
/// # Operation Modes
///
/// ## Legacy Mode
/// - **V1 Receipts**: Accept and process V1 TAP receipts only
/// - **V1 RAVs**: Generate V1 Receipt Aggregate Vouchers (RAVs)
/// - **V2 Support**: V2 receipts are rejected
/// - **Use Case**: Pure legacy indexer operations before Horizon migration
///
/// ## Horizon Mode (Hybrid)
/// - **V2 Receipts**: Accept new V2 TAP receipts (primary mode)
/// - **V1 Receipts**: Continue processing existing V1 receipts for RAV generation
/// - **V1 Submissions**: Reject new V1 receipt submissions
/// - **V2 RAVs**: Generate V2 Receipt Aggregate Vouchers using SubgraphService
/// - **Use Case**: Horizon migration period with hybrid V1/V2 support
///
/// # Configuration Mapping
///
/// This enum is derived from the `horizon.enabled` flag in the configuration.
/// Horizon mode requires `blockchain.subgraph_service_address`.
///
/// ```toml
/// # Legacy mode (default)
/// [horizon]
/// enabled = false
///
/// # Horizon mode
/// [horizon]
/// enabled = true
///
/// [blockchain]
/// subgraph_service_address = "0x..."
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub enum TapMode {
    /// Legacy TAP mode - V1 receipts and RAVs only
    ///
    /// In this mode:
    /// - Only V1 TAP receipts are accepted and processed
    /// - V1 RAVs are generated using legacy aggregator endpoints
    /// - V2 receipts are rejected with an error
    /// - No SubgraphService integration required
    Legacy,

    /// Horizon TAP mode - Hybrid V1/V2 support with V2 infrastructure
    ///
    /// In this mode:
    /// - **Primary**: Accept and process new V2 TAP receipts
    /// - **Legacy**: Continue processing existing V1 receipts for RAV generation
    /// - **Rejection**: Reject new V1 receipt submissions
    /// - **Infrastructure**: V2 operations require SubgraphService integration
    ///
    /// The `subgraph_service_address` is used for:
    /// - V2 receipt verification against SubgraphService contract
    /// - V2 RAV generation and validation
    /// - Query routing for V2 operations
    Horizon {
        /// Address of the SubgraphService contract used for V2 operations
        ///
        /// This address is required for all V2 TAP receipt operations including:
        /// - Receipt signature verification
        /// - RAV generation requests to aggregator
        /// - Query validation and routing
        subgraph_service_address: Address,
    },
}

impl TapMode {
    /// Check if the indexer is operating in Horizon mode
    ///
    /// Returns `true` if V2 TAP receipts are supported, `false` otherwise.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// # use thegraph_core::alloy::primitives::Address;
    /// let mode = TapMode::Horizon {
    ///     subgraph_service_address: Address::ZERO
    /// };
    /// assert!(mode.is_horizon());
    ///
    /// let mode = TapMode::Legacy;
    /// assert!(!mode.is_horizon());
    /// ```
    pub fn is_horizon(&self) -> bool {
        matches!(self, TapMode::Horizon { .. })
    }

    /// Check if the indexer is operating in Legacy mode
    ///
    /// Returns `true` if only V1 TAP receipts are supported, `false` otherwise.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// let mode = TapMode::Legacy;
    /// assert!(mode.is_legacy());
    /// ```
    pub fn is_legacy(&self) -> bool {
        matches!(self, TapMode::Legacy)
    }

    /// Get the SubgraphService address if in Horizon mode
    ///
    /// Returns `Some(Address)` in Horizon mode, `None` in Legacy mode.
    /// Use this when you need to conditionally access V2 infrastructure.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// # use thegraph_core::alloy::primitives::Address;
    /// let mode = TapMode::Horizon {
    ///     subgraph_service_address: Address::ZERO
    /// };
    /// assert_eq!(mode.subgraph_service_address(), Some(Address::ZERO));
    ///
    /// let mode = TapMode::Legacy;
    /// assert_eq!(mode.subgraph_service_address(), None);
    /// ```
    pub fn subgraph_service_address(&self) -> Option<Address> {
        match self {
            TapMode::Legacy => None,
            TapMode::Horizon {
                subgraph_service_address,
            } => Some(*subgraph_service_address),
        }
    }

    /// Get the SubgraphService address, panicking if in Legacy mode
    ///
    /// Use this when you know you're in a V2/Horizon context and the address
    /// should always be available. Panics with a descriptive message if called
    /// in Legacy mode.
    ///
    /// # Panics
    ///
    /// Panics if called on `TapMode::Legacy`.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// # use thegraph_core::alloy::primitives::Address;
    /// let mode = TapMode::Horizon {
    ///     subgraph_service_address: Address::ZERO
    /// };
    /// assert_eq!(mode.require_subgraph_service_address(), Address::ZERO);
    /// ```
    ///
    /// ```should_panic
    /// # use indexer_config::TapMode;
    /// let mode = TapMode::Legacy;
    /// mode.require_subgraph_service_address(); // Panics!
    /// ```
    pub fn require_subgraph_service_address(&self) -> Address {
        match self {
            TapMode::Legacy => {
                panic!(
                    "Attempted to access subgraph_service_address in Legacy mode. \
                       Check tap_mode.is_horizon() before calling this method."
                )
            }
            TapMode::Horizon {
                subgraph_service_address,
            } => *subgraph_service_address,
        }
    }

    /// Check if V2 TAP receipts are supported
    ///
    /// Alias for [`is_horizon()`](Self::is_horizon) with more explicit naming.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// # use thegraph_core::alloy::primitives::Address;
    /// let mode = TapMode::Horizon {
    ///     subgraph_service_address: Address::ZERO
    /// };
    /// assert!(mode.supports_v2());
    /// ```
    pub fn supports_v2(&self) -> bool {
        self.is_horizon()
    }

    /// Check if only V1 TAP receipts are supported
    ///
    /// Returns `true` if V2 receipts should be rejected.
    ///
    /// # Example
    /// ```rust
    /// # use indexer_config::TapMode;
    /// let mode = TapMode::Legacy;
    /// assert!(mode.v1_only());
    /// ```
    pub fn v1_only(&self) -> bool {
        self.is_legacy()
    }
}

/// Configuration for the Horizon migration
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct HorizonConfig {
    /// Enable Horizon migration support and detection
    /// When enabled, set `blockchain.subgraph_service_address` and
    /// `blockchain.receipts_verifier_address_v2`

    /// When disabled: Pure legacy mode, no Horizon detection performed
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap, HashSet},
        env, fs,
        path::PathBuf,
        str::FromStr,
    };

    use figment::value::Uncased;
    use sealed_test::prelude::*;
    use thegraph_core::alloy::primitives::{address, Address, FixedBytes, U256};
    use tracing_test::traced_test;

    use super::{DatabaseConfig, SHARED_PREFIX};
    use crate::{Config, ConfigPrefix};

    #[test]
    fn test_minimal_config() {
        Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from("minimal-config-example.toml")).as_ref(),
        )
        .unwrap();
    }

    #[test]
    fn test_maximal_config() {
        // Generate full config by deserializing the minimal config and let the code fill in the defaults.
        let mut max_config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from("minimal-config-example.toml")).as_ref(),
        )
        .unwrap();
        max_config.tap.trusted_senders =
            HashSet::from([address!("deadbeefcafebabedeadbeefcafebabedeadbeef")]);
        max_config.dips = Some(crate::DipsConfig {
            allowed_payers: vec![Address(
                FixedBytes::<20>::from_str("0x3333333333333333333333333333333333333333").unwrap(),
            )],
            price_per_entity: U256::from(1000),
            price_per_epoch: BTreeMap::from_iter(vec![
                ("mainnet".to_string(), U256::from(100)),
                ("hardhat".to_string(), U256::from(100)),
            ]),
            additional_networks: HashMap::from([(
                "eip155:1337".to_string(),
                "hardhat".to_string(),
            )]),
            ..Default::default()
        });

        let max_config_file: Config = toml::from_str(
            fs::read_to_string("maximal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();

        assert_eq!(max_config, max_config_file);
    }

    // Test that we can load config with unknown fields, in particular coming from environment variables
    #[sealed_test(files = ["minimal-config-example.toml"])]
    #[traced_test]
    fn test_unknown_fields() {
        // Add environment variable that would correspond to an unknown field
        std::env::set_var("INDEXER_SERVICE_PLUMBUS", "howisitmade?");

        Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from("minimal-config-example.toml")).as_ref(),
        )
        .unwrap();

        assert!(logs_contain(
            "Ignoring unknown configuration field: plumbus"
        ));
    }

    // Test that we can fill in mandatory config fields missing from the config file with
    // environment variables
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_fill_in_missing_with_env() {
        let mut minimal_config: toml::Value = toml::from_str(
            fs::read_to_string("minimal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();
        // Remove the subgraphs.network.query_url field from minimal config
        minimal_config
            .get_mut("subgraphs")
            .unwrap()
            .get_mut("network")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .remove("query_url");

        // Save the modified minimal config to a named temporary file using tempfile
        let temp_minimal_config_path = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp_minimal_config_path.path(),
            toml::to_string(&minimal_config).unwrap(),
        )
        .unwrap();

        // This should fail because the subgraphs.network.query_url field is missing
        Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap_err();

        let test_value = "http://localhost:8000/testvalue";
        env::set_var("INDEXER_SERVICE_SUBGRAPHS__NETWORK__QUERY_URL", test_value);

        let config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap();

        assert_eq!(
            config.subgraphs.network.config.query_url.as_str(),
            test_value
        );
    }

    // Test that we can override nested config values with environment variables
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_override_with_env() {
        let test_value = "http://localhost:8000/testvalue";
        env::set_var("INDEXER_SERVICE_SUBGRAPHS__NETWORK__QUERY_URL", test_value);

        let config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from("minimal-config-example.toml")).as_ref(),
        )
        .unwrap();

        assert_eq!(
            config.subgraphs.network.config.query_url.as_str(),
            test_value
        );
    }

    #[test]
    fn test_ignore_empty_values() {
        env::set_var("INDEXER_TEST1", "123");
        env::set_var("INDEXER_TEST2", "");
        env::set_var("INDEXER_TEST3__TEST1", "123");
        env::set_var("INDEXER_TEST3__TEST2", "");

        let env = Config::from_env_ignore_empty(SHARED_PREFIX);

        let values: Vec<_> = env.iter().collect();

        assert_eq!(values.len(), 2);

        assert_eq!(values[0], (Uncased::new("test1"), "123".to_string()));
        assert_eq!(values[1], (Uncased::new("test3.test1"), "123".to_string()));
    }

    // Test to check substitute_env_vars function is substituting env variables
    // indexers can use ${ENV_VAR_NAME} to point to the required env variable
    #[test]
    fn test_substitution_using_regex() {
        // Set up environment variables
        env::set_var("TEST_VAR1", "changed_value_1");

        let input = r#"
            [section1]
            key1 = "${TEST_VAR1}"
            key2 = "${TEST_VAR-default}"
            key3 = "{{TEST_VAR3}}"

            [section2]
            key4 = "prefix_${TEST_VAR1}_${TEST_VAR-default}_suffix"
            key5 = "a_key_without_substitution"
        "#
        .to_string();

        let expected_output = r#"
            [section1]
            key1 = "changed_value_1"
            key2 = "${TEST_VAR-default}"
            key3 = "{{TEST_VAR3}}"

            [section2]
            key4 = "prefix_changed_value_1_${TEST_VAR-default}_suffix"
            key5 = "a_key_without_substitution"
        "#
        .to_string();

        let result = Config::substitute_env_vars(input).expect("error substiting env variables");

        assert_eq!(
            result.trim(),
            expected_output.trim(),
            "Environment variable substitution failed"
        );

        // Clean up environment variables
        env::remove_var("TEST_VAR1");
    }
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_parse_with_env_substitution_and_overrides() {
        let mut minimal_config: toml::Value = toml::from_str(
            fs::read_to_string("minimal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();
        // Change the subgraphs query_url to an env variable
        minimal_config
            .get_mut("subgraphs")
            .unwrap()
            .get_mut("network")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert(
                String::from("query_url"),
                toml::Value::String("${QUERY_URL}".to_string()),
            );

        // Save the modified minimal config to a named temporary file using tempfile
        let temp_minimal_config_path = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp_minimal_config_path.path(),
            toml::to_string(&minimal_config).unwrap(),
        )
        .unwrap();

        // This should fail because the QUERY_URL env variable is not setup
        Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap_err();

        let test_value = "http://localhost:8000/testvalue";
        env::set_var("QUERY_URL", test_value);

        let config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap();

        assert_eq!(
            config.subgraphs.network.config.query_url.as_str(),
            test_value
        );
    }
    #[test]
    fn test_url_format() {
        let data = DatabaseConfig::PostgresVars {
            host: String::from("postgres"),
            port: Some(1234),
            user: String::from("postgres"),
            password: Some(String::from("postgres")),
            database: String::from("postgres"),
        };
        let formated_data = data.get_formated_postgres_url();
        assert_eq!(
            formated_data.as_str(),
            "postgres://postgres:postgres@postgres:1234/postgres"
        );

        let data = DatabaseConfig::PostgresVars {
            host: String::from("postgres"),
            port: None,
            user: String::from("postgres"),
            password: None,
            database: String::from("postgres"),
        };
        let formated_data = data.get_formated_postgres_url();
        assert_eq!(
            formated_data.as_str(),
            "postgres://postgres@postgres/postgres"
        );
    }

    // Test that we can fill in mandatory config fields missing from the config file with
    // environment variables
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_change_db_config_with_individual_vars() {
        let mut minimal_config: toml::Value = toml::from_str(
            fs::read_to_string("minimal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();
        // Remove the database.postgres_url field from minimal config
        minimal_config
            .get_mut("database")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .remove("postgres_url");

        let database_table = minimal_config
            .get_mut("database")
            .unwrap()
            .as_table_mut()
            .unwrap();
        database_table.insert(
            "host".to_string(),
            toml::Value::String("postgres".to_string()),
        );
        database_table.insert(
            "user".to_string(),
            toml::Value::String("postgres".to_string()),
        );
        database_table.insert(
            "database".to_string(),
            toml::Value::String("postgres".to_string()),
        );

        // Save the modified minimal config to a named temporary file using tempfile
        let temp_minimal_config_path = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp_minimal_config_path.path(),
            toml::to_string(&minimal_config).unwrap(),
        )
        .unwrap();

        // Parse the config with new datbase vars
        let config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap();

        assert_eq!(
            config.database.get_formated_postgres_url().as_str(),
            "postgres://postgres@postgres/postgres"
        );
    }

    // Test that we can fill in mandatory config fields missing from the config file with
    // environment variables
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_fill_in_missing_with_shared_env() {
        let mut minimal_config: toml::Value = toml::from_str(
            fs::read_to_string("minimal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();
        // Remove the database.postgres_url field from minimal config
        minimal_config
            .get_mut("database")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .remove("postgres_url");

        // Save the modified minimal config to a named temporary file using tempfile
        let temp_minimal_config_path = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp_minimal_config_path.path(),
            toml::to_string(&minimal_config).unwrap(),
        )
        .unwrap();

        // No need to parse since from another test we know parsing at this point it will fail

        let test_value = "postgres://postgres@postgres:5432/postgres";
        env::set_var("INDEXER_DATABASE__POSTGRES_URL", test_value);

        let config = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_minimal_config_path.path())).as_ref(),
        )
        .unwrap();

        assert_eq!(
            config.database.get_formated_postgres_url().as_str(),
            test_value
        );
    }
}
