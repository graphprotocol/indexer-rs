// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
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
        // Validate that at least one operator mnemonic is configured
        if self.indexer.operator_mnemonic.is_none()
            && self
                .indexer
                .operator_mnemonics
                .as_ref()
                .is_none_or(|v| v.is_empty())
        {
            return Err("No operator mnemonic configured. \
                Set either `indexer.operator_mnemonic` or `indexer.operator_mnemonics`."
                .to_string());
        }

        // Warn if the same mnemonic appears in both fields (will be deduplicated)
        if let (Some(singular), Some(plural)) = (
            &self.indexer.operator_mnemonic,
            &self.indexer.operator_mnemonics,
        ) {
            if plural.contains(singular) {
                tracing::warn!(
                    "The same mnemonic appears in both `operator_mnemonic` and \
                    `operator_mnemonics`. The duplicate will be ignored."
                );
            }
        }

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
        // 0.1 GRT in wei = 0.1 * 10^18 = 100_000_000_000_000_000
        const MINIMUM_TRIGGER_VALUE_WEI: u128 = 100_000_000_000_000_000;
        if trigger_value < MINIMUM_TRIGGER_VALUE_WEI {
            tracing::warn!(
                "Trigger value is too low, currently below 0.1 GRT. \
                Please modify `max_amount_willing_to_lose_grt` or `trigger_value_divisor`. \
                It is best to have a higher trigger value, ideally above 1 GRT. \
                Anything lower and the system may constantly deny the sender. \
                `Trigger value`  is defined by: \
                (max_amount_willing_to_lose_grt / trigger_value_divisor) "
            )
        }

        // 0.001 GRT in wei = 0.001 * 10^18 = 1_000_000_000_000_000
        // This represents approximately 100x a typical query price (0.00001 GRT)
        const MINIMUM_MAX_WILLING_TO_LOSE_WEI: u128 = 1_000_000_000_000_000;
        if self.tap.max_amount_willing_to_lose_grt.get_value() < MINIMUM_MAX_WILLING_TO_LOSE_WEI {
            tracing::warn!(
                "Your `max_amount_willing_to_lose_grt` value is too close to zero. \
                This may deny the sender too often or even break the whole system. \
                It's recommended it to be a value greater than 100x an usual query price."
            );
        }

        // Validate syncing_interval_secs is not zero
        if self.subgraphs.escrow.config.syncing_interval_secs == Duration::ZERO {
            return Err(
                "subgraphs.escrow.syncing_interval_secs must be greater than 0".to_string(),
            );
        }
        if self.subgraphs.network.config.syncing_interval_secs == Duration::ZERO {
            return Err(
                "subgraphs.network.syncing_interval_secs must be greater than 0".to_string(),
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

        // Validate request_timeout_secs is not zero
        if self.tap.rav_request.request_timeout_secs == Duration::ZERO {
            return Err("tap.rav_request.request_timeout_secs must be greater than 0".to_string());
        }

        // Validate sender_timeout_secs is not zero
        if self.tap.sender_timeout_secs == Duration::ZERO {
            return Err("tap.sender_timeout_secs must be greater than 0".to_string());
        }

        // Validate recently_closed_allocation_buffer_secs is not zero
        if self
            .subgraphs
            .network
            .recently_closed_allocation_buffer_secs
            == Duration::ZERO
        {
            return Err(
                "subgraphs.network.recently_closed_allocation_buffer_secs must be greater than 0"
                    .to_string(),
            );
        }

        if self.tap.allocation_reconciliation_interval_secs == Duration::ZERO {
            return Err(
                "tap.allocation_reconciliation_interval_secs must be greater than 0".to_string(),
            );
        }

        if self.tap.allocation_reconciliation_interval_secs < Duration::from_secs(60) {
            tracing::warn!(
                "Your `tap.allocation_reconciliation_interval_secs` value is too low. \
                This may cause unnecessary load on the system. \
                A recommended value is at least 60 seconds."
            );
        }

        // Warn about auth tokens over cleartext HTTP (TRST-L-3)
        // This is a security risk as tokens can be intercepted
        Self::warn_if_token_over_http(
            &self.subgraphs.network.config.query_url,
            self.subgraphs.network.config.query_auth_token.as_ref(),
            "subgraphs.network",
        );
        Self::warn_if_token_over_http(
            &self.subgraphs.escrow.config.query_url,
            self.subgraphs.escrow.config.query_auth_token.as_ref(),
            "subgraphs.escrow",
        );

        // Horizon configuration validation (required).
        if !self.horizon.enabled {
            return Err("Horizon is required; set [horizon].enabled = true".to_string());
        }
        if self.blockchain.subgraph_service_address.is_none() {
            return Err(
                "Horizon is required; set `blockchain.subgraph_service_address`".to_string(),
            );
        }
        if self.blockchain.receipts_verifier_address_v2.is_none() {
            return Err(
                "Horizon is required; set `blockchain.receipts_verifier_address_v2`".to_string(),
            );
        }

        Ok(())
    }

    /// Warns if an authentication token is configured with a non-HTTPS URL.
    ///
    /// Sending bearer tokens over cleartext HTTP exposes them to interception
    /// via man-in-the-middle attacks. This validation helps catch insecure
    /// configurations before they cause credential leakage.
    fn warn_if_token_over_http(url: &Url, token: Option<&String>, config_path: &str) {
        if let Some(token) = token {
            if !token.is_empty() && url.scheme() != "https" {
                // Allow localhost/127.0.0.1 for development without warning
                let is_localhost = url
                    .host_str()
                    .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "::1");

                if !is_localhost {
                    tracing::warn!(
                        config_path,
                        url = %url,
                        "Authentication token configured with non-HTTPS URL. \
                        This may expose credentials to interception. \
                        Use HTTPS for production deployments."
                    );
                }
            }
        }
    }

    /// Derive TAP operation mode from horizon configuration.
    ///
    /// This method translates the `[horizon]` configuration section into a
    /// [`TapMode`] enum for use throughout the indexer codebase.
    ///
    /// # Returns
    ///
    /// - [`TapMode::Horizon`] when `horizon.enabled = true` with the configured
    ///   `blockchain.subgraph_service_address`
    pub fn tap_mode(&self) -> TapMode {
        TapMode::Horizon {
            subgraph_service_address: self
                .blockchain
                .subgraph_service_address
                .expect("subgraph_service_address should be validated during Config::validate()"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct IndexerConfig {
    pub indexer_address: Address,
    /// Single operator mnemonic.
    /// Use `operator_mnemonics` for multiple mnemonics.
    #[serde(default)]
    pub operator_mnemonic: Option<Mnemonic>,
    /// Multiple operator mnemonics for supporting allocations created
    /// with different operator keys (e.g., after key rotation or migration).
    #[serde(default)]
    pub operator_mnemonics: Option<Vec<Mnemonic>>,
}

impl IndexerConfig {
    /// Get all configured operator mnemonics.
    ///
    /// Returns mnemonics from both `operator_mnemonic` (singular) and
    /// `operator_mnemonics` (plural) fields combined.
    ///
    /// Note: Config validation ensures at least one mnemonic is configured
    /// before this method is called. Returns an empty Vec only if called
    /// on an unvalidated config.
    pub fn get_operator_mnemonics(&self) -> Vec<Mnemonic> {
        let mut mnemonics = Vec::new();

        if let Some(ref mnemonic) = self.operator_mnemonic {
            mnemonics.push(mnemonic.clone());
        }

        if let Some(ref additional) = self.operator_mnemonics {
            for m in additional {
                // Avoid duplicates if the same mnemonic is in both fields
                if !mnemonics.iter().any(|existing| existing == m) {
                    mnemonics.push(m.clone());
                }
            }
        }

        mnemonics
    }
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

    /// Maximum allowed age of network subgraph data in minutes.
    /// Responses older than this are rejected to prevent stale data from replacing fresh data.
    /// This protects against Gateway routing queries to indexers that are significantly behind.
    /// Set to 0 to disable staleness checking.
    /// Default: 30 (minutes)
    #[serde(default = "default_max_data_staleness_mins")]
    pub max_data_staleness_mins: u64,
}

fn default_max_data_staleness_mins() -> u64 {
    30
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
    /// Legacy verifier address (deprecated; optional, not used).
    #[deprecated(note = "Use `receipts_verifier_address_v2` for Horizon receipts.")]
    pub receipts_verifier_address: Option<Address>,
    /// Verifier address for Horizon receipts.
    pub receipts_verifier_address_v2: Option<Address>,
    /// Address of the SubgraphService contract used for Horizon operations
    pub subgraph_service_address: Option<Address>,
}

impl BlockchainConfig {
    pub fn horizon_receipts_verifier_address(&self) -> Address {
        self.receipts_verifier_address_v2
            .expect("receipts_verifier_address_v2 should be validated during Config::validate()")
    }
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
    /// Maximum number of deployments allowed in a single costModels batch query.
    /// Prevents DoS attacks via unbounded input.
    /// Default: 200
    #[serde(default = "default_max_cost_model_batch_size")]
    pub max_cost_model_batch_size: usize,
    /// Maximum request body size in bytes for query endpoints.
    /// Prevents DoS attacks via unbounded request buffering (TRST-M-6).
    /// Default: 2MB (2_097_152 bytes)
    #[serde(default = "default_max_request_body_size")]
    pub max_request_body_size: usize,
}

fn default_max_cost_model_batch_size() -> usize {
    200
}

/// Default max request body size: 2MB
/// GraphQL queries are typically small, but can include large variable payloads.
fn default_max_request_body_size() -> usize {
    2 * 1024 * 1024
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
    ///
    /// Allocation state is normally updated via watcher events from the network subgraph.
    /// However, if connectivity to the subgraph is lost, allocation closure events may be
    /// missed. This periodic reconciliation forces a re-check of all allocations against
    /// the current subgraph state, ensuring stale allocations are detected and processed
    /// even after connectivity failures.
    ///
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

/// TAP protocol operation mode.
///
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub enum TapMode {
    /// Horizon TAP mode.
    Horizon {
        /// Address of the SubgraphService contract used for Horizon operations.
        subgraph_service_address: Address,
    },
}

impl TapMode {
    /// Check if the indexer is operating in Horizon mode
    ///
    /// Returns `true` when Horizon is enabled, `false` otherwise.
    pub fn is_horizon(&self) -> bool {
        matches!(self, TapMode::Horizon { .. })
    }

    /// Get the SubgraphService address for Horizon mode.
    pub fn subgraph_service_address(&self) -> Address {
        self.require_subgraph_service_address()
    }

    /// Get the SubgraphService address, panicking if not in Horizon mode.
    pub fn require_subgraph_service_address(&self) -> Address {
        match self {
            TapMode::Horizon {
                subgraph_service_address,
            } => *subgraph_service_address,
        }
    }

    /// Check if Horizon receipts are supported.
    ///
    /// Alias for [`is_horizon()`](Self::is_horizon) with more explicit naming.
    pub fn supports_v2(&self) -> bool {
        self.is_horizon()
    }
}

/// Configuration for Horizon support.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct HorizonConfig {
    /// Enable Horizon support and detection.
    /// When enabled, set `blockchain.subgraph_service_address` and
    /// `blockchain.receipts_verifier_address_v2`.
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

    use bip39::Mnemonic;
    use figment::value::Uncased;
    use sealed_test::prelude::*;
    use thegraph_core::alloy::primitives::{address, Address, FixedBytes, U256};
    use tracing_test::traced_test;

    use super::{DatabaseConfig, IndexerConfig, SHARED_PREFIX};
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

    const MNEMONIC_1: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MNEMONIC_2: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";
    const MNEMONIC_3: &str =
        "legal winner thank year wave sausage worth useful legal winner thank yellow";

    /// Test that duplicate mnemonics in both operator_mnemonic and operator_mnemonics
    /// are deduplicated.
    #[test]
    fn test_get_operator_mnemonics_deduplication() {
        let mnemonic = Mnemonic::from_str(MNEMONIC_1).unwrap();

        // Same mnemonic in both fields
        let config = IndexerConfig {
            indexer_address: Address::ZERO,
            operator_mnemonic: Some(mnemonic.clone()),
            operator_mnemonics: Some(vec![mnemonic.clone()]),
        };

        let result = config.get_operator_mnemonics();

        assert_eq!(
            result.len(),
            1,
            "Duplicate mnemonics should be deduplicated"
        );
        assert_eq!(result[0], mnemonic);
    }

    /// Test that order is preserved: singular field first, then plural field entries.
    #[test]
    fn test_get_operator_mnemonics_order_preserved() {
        let mnemonic_1 = Mnemonic::from_str(MNEMONIC_1).unwrap();
        let mnemonic_2 = Mnemonic::from_str(MNEMONIC_2).unwrap();
        let mnemonic_3 = Mnemonic::from_str(MNEMONIC_3).unwrap();

        let config = IndexerConfig {
            indexer_address: Address::ZERO,
            operator_mnemonic: Some(mnemonic_1.clone()),
            operator_mnemonics: Some(vec![mnemonic_2.clone(), mnemonic_3.clone()]),
        };

        let result = config.get_operator_mnemonics();

        assert_eq!(result.len(), 3, "Should have 3 distinct mnemonics");
        assert_eq!(
            result[0], mnemonic_1,
            "First should be from operator_mnemonic"
        );
        assert_eq!(
            result[1], mnemonic_2,
            "Second should be first from operator_mnemonics"
        );
        assert_eq!(
            result[2], mnemonic_3,
            "Third should be second from operator_mnemonics"
        );
    }

    /// Test combining both fields with partial overlap produces correct merged result.
    #[test]
    fn test_get_operator_mnemonics_combined_with_overlap() {
        let mnemonic_1 = Mnemonic::from_str(MNEMONIC_1).unwrap();
        let mnemonic_2 = Mnemonic::from_str(MNEMONIC_2).unwrap();
        let mnemonic_3 = Mnemonic::from_str(MNEMONIC_3).unwrap();

        // mnemonic_1 is in both fields (should be deduplicated)
        // mnemonic_2 and mnemonic_3 are only in operator_mnemonics
        let config = IndexerConfig {
            indexer_address: Address::ZERO,
            operator_mnemonic: Some(mnemonic_1.clone()),
            operator_mnemonics: Some(vec![
                mnemonic_1.clone(), // duplicate
                mnemonic_2.clone(),
                mnemonic_3.clone(),
            ]),
        };

        let result = config.get_operator_mnemonics();

        assert_eq!(
            result.len(),
            3,
            "Should have 3 mnemonics after deduplication"
        );
        assert_eq!(result[0], mnemonic_1, "First should be mnemonic_1");
        assert_eq!(result[1], mnemonic_2, "Second should be mnemonic_2");
        assert_eq!(result[2], mnemonic_3, "Third should be mnemonic_3");
    }

    /// Test that only operator_mnemonic (singular) works correctly.
    #[test]
    fn test_get_operator_mnemonics_singular_only() {
        let mnemonic = Mnemonic::from_str(MNEMONIC_1).unwrap();

        let config = IndexerConfig {
            indexer_address: Address::ZERO,
            operator_mnemonic: Some(mnemonic.clone()),
            operator_mnemonics: None,
        };

        let result = config.get_operator_mnemonics();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], mnemonic);
    }

    /// Test that only operator_mnemonics (plural) works correctly.
    #[test]
    fn test_get_operator_mnemonics_plural_only() {
        let mnemonic_1 = Mnemonic::from_str(MNEMONIC_1).unwrap();
        let mnemonic_2 = Mnemonic::from_str(MNEMONIC_2).unwrap();

        let config = IndexerConfig {
            indexer_address: Address::ZERO,
            operator_mnemonic: None,
            operator_mnemonics: Some(vec![mnemonic_1.clone(), mnemonic_2.clone()]),
        };

        let result = config.get_operator_mnemonics();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], mnemonic_1);
        assert_eq!(result[1], mnemonic_2);
    }

    /// Test that config validation rejects when no operator mnemonics are configured.
    #[sealed_test(files = ["minimal-config-example.toml"])]
    fn test_validation_rejects_missing_operator_mnemonics() {
        let mut minimal_config: toml::Value = toml::from_str(
            fs::read_to_string("minimal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();

        // Remove operator_mnemonic from the config
        minimal_config
            .get_mut("indexer")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .remove("operator_mnemonic");

        // Save to temp file
        let temp_config_path = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp_config_path.path(),
            toml::to_string(&minimal_config).unwrap(),
        )
        .unwrap();

        // Parse should fail due to validation
        let result = Config::parse(
            ConfigPrefix::Service,
            Some(PathBuf::from(temp_config_path.path())).as_ref(),
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("No operator mnemonic configured"));
    }
}
