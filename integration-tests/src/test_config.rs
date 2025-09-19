// Copyright 2025-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{env, fs, path::Path, process::Command};

use anyhow::Result;
use serde::Deserialize;

use crate::{
    constants::{
        ACCOUNT0_ADDRESS, ACCOUNT0_SECRET, ACCOUNT1_ADDRESS, ACCOUNT1_SECRET, CHAIN_ID,
        GATEWAY_API_KEY, GATEWAY_URL, GRAPH_TALLY_COLLECTOR_CONTRACT, GRAPH_URL, INDEXER_URL,
        SUBGRAPH_ID, TAP_AGENT_METRICS_URL, TAP_VERIFIER_CONTRACT, TEST_DATA_SERVICE,
        TEST_SUBGRAPH_DEPLOYMENT,
    },
    env_loader::load_integration_env,
};

/// Simple struct to hold just the TAP values we need from the tap-agent config
#[derive(Debug, Clone)]
pub struct TapConfig {
    pub max_amount_willing_to_lose_grt: f64,
    pub trigger_value_divisor: f64,
    pub timestamp_buffer_secs: u64,
}

impl TapConfig {
    pub fn get_trigger_value(&self) -> u128 {
        let grt_wei = (self.max_amount_willing_to_lose_grt * 1e18) as u128;
        (grt_wei as f64 / self.trigger_value_divisor) as u128
    }
}

#[derive(Clone)]
pub struct TestConfig {
    pub indexer_url: String,
    pub gateway_url: String,
    pub graph_url: String,
    pub tap_agent_metrics_url: String,
    pub database_url: String,

    pub chain_id: u64,
    pub gateway_api_key: String,

    pub account0_address: String,
    pub account0_secret: String,
    pub account1_address: String,
    pub account1_secret: String,

    pub tap_verifier_contract: String,
    pub graph_tally_collector_contract: String,

    pub subgraph_id: String,
    pub test_subgraph_deployment: String,

    pub test_data_service: String,

    // Cached tap agent configuration values
    tap_values: Option<TapConfig>,
}

#[derive(Deserialize)]
struct TapContracts {
    #[serde(rename = "1337")]
    chain: Option<TapContractsChain>,
}

#[derive(Deserialize)]
struct TapContractsChain {
    #[serde(rename = "TAPVerifier")]
    tap_verifier: Option<String>,
}

#[derive(Deserialize)]
struct HorizonJson {
    #[serde(rename = "1337")]
    chain: Option<HorizonChain>,
}

#[derive(Deserialize)]
struct HorizonChain {
    #[serde(rename = "GraphTallyCollector")]
    gtc: Option<AddressOnly>,
}

#[derive(Deserialize)]
struct AddressOnly {
    address: String,
}

#[derive(Deserialize)]
struct SubgraphServiceJson {
    #[serde(rename = "1337")]
    chain: Option<SubgraphServiceChain>,
}

#[derive(Deserialize)]
struct SubgraphServiceChain {
    #[serde(rename = "SubgraphService")]
    svc: Option<AddressOnly>,
}

impl TestConfig {
    pub fn from_env() -> Result<Self> {
        // Ensure integration-tests/.env is loaded (symlink to contrib/local-network/.env)
        load_integration_env();

        let get_u64 = |k: &str, d: u64| -> u64 {
            env::var(k)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(d)
        };
        let get_s = |k: &str, d: &str| -> String { env::var(k).unwrap_or_else(|_| d.to_string()) };

        // Derive URLs from ports when present, allow explicit URL overrides
        let indexer_url = env::var("INDEXER_URL").unwrap_or_else(|_| INDEXER_URL.to_string());
        let gateway_url = env::var("GATEWAY_URL").unwrap_or_else(|_| GATEWAY_URL.to_string());

        let graph_url = env::var("GRAPH_URL").unwrap_or_else(|_| GRAPH_URL.to_string());

        let tap_agent_metrics_url = get_s("TAP_AGENT_METRICS_URL", TAP_AGENT_METRICS_URL);

        let chain_id = get_u64("CHAIN_ID", CHAIN_ID);
        let gateway_api_key = get_s("GATEWAY_API_KEY", GATEWAY_API_KEY);

        // Database URL with fallback to constants::POSTGRES_URL
        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| crate::constants::POSTGRES_URL.to_string());

        // TODO: default values from constants or load contrib/local-network/.env
        let account0_address = get_s("ACCOUNT0_ADDRESS", ACCOUNT0_ADDRESS);
        let account0_secret = get_s("ACCOUNT0_SECRET", ACCOUNT0_SECRET);
        let account1_address = get_s("ACCOUNT1_ADDRESS", ACCOUNT1_ADDRESS);
        let account1_secret = get_s("ACCOUNT1_SECRET", ACCOUNT1_SECRET);

        let mut tap_verifier_contract = TAP_VERIFIER_CONTRACT.to_string();
        let mut graph_tally_collector_contract = GRAPH_TALLY_COLLECTOR_CONTRACT.to_string();
        let mut test_data_service = TEST_DATA_SERVICE.to_string();

        // Load JSONs from contrib when available. Path is relative to integration-tests/ CWD.
        read_tap_verifier_json("../contrib/local-network/tap-contracts.json")
            .map(|addr| tap_verifier_contract = addr)
            .ok();
        read_graph_tally_collector_json("../contrib/local-network/horizon.json")
            .map(|addr| graph_tally_collector_contract = addr)
            .ok();
        read_subgraph_service_json("../contrib/local-network/subgraph-service.json")
            .map(|addr| test_data_service = addr)
            .ok();

        // Subgraph identifiers
        let subgraph_id = get_s("SUBGRAPH", SUBGRAPH_ID);
        // Allow env override; also accept alias SUBGRAPH_DEPLOYMENT. Fallback to constants::TEST_SUBGRAPH_DEPLOYMENT.
        let test_subgraph_deployment = env::var("TEST_SUBGRAPH_DEPLOYMENT")
            .unwrap_or_else(|_| TEST_SUBGRAPH_DEPLOYMENT.to_string());

        Ok(Self {
            indexer_url,
            gateway_url,
            graph_url,
            tap_agent_metrics_url,
            database_url,
            chain_id,
            gateway_api_key,
            account0_address,
            account0_secret,
            account1_address,
            account1_secret,
            tap_verifier_contract,
            graph_tally_collector_contract,
            subgraph_id,
            test_subgraph_deployment,
            test_data_service,
            tap_values: None, // Will be loaded on-demand
        })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Get the tap-agent configuration from the running Docker container
    pub fn get_tap_config(&mut self) -> Result<&TapConfig> {
        if self.tap_values.is_none() {
            let config_content = Self::extract_tap_agent_config_from_docker()?;
            let tap_config = Self::parse_tap_values(&config_content)?;
            self.tap_values = Some(tap_config);
        }
        Ok(self.tap_values.as_ref().unwrap())
    }

    /// Extract the configuration file from the running tap-agent Docker container
    fn extract_tap_agent_config_from_docker() -> Result<String> {
        let output = Command::new("docker")
            .args(["exec", "tap-agent", "cat", "/opt/config.toml"])
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to execute docker command: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Docker exec failed: {}", stderr));
        }

        let config_content = String::from_utf8(output.stdout)
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in config file: {}", e))?;

        Ok(config_content)
    }

    /// Parse only the TAP values we need from the config
    fn parse_tap_values(config_content: &str) -> Result<TapConfig> {
        let parsed: toml::Value = toml::from_str(config_content)?;

        // Extract the values we need
        let tap_section = parsed
            .get("tap")
            .ok_or_else(|| anyhow::anyhow!("No [tap] section found in config"))?;

        let max_amount_willing_to_lose_grt = tap_section
            .get("max_amount_willing_to_lose_grt")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(1.0);

        let rav_request_section = tap_section
            .get("rav_request")
            .ok_or_else(|| anyhow::anyhow!("No [tap.rav_request] section found"))?;

        let trigger_value_divisor = rav_request_section
            .get("trigger_value_divisor")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(10.0);

        let timestamp_buffer_secs = rav_request_section
            .get("timestamp_buffer_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(15) as u64;

        Ok(TapConfig {
            max_amount_willing_to_lose_grt,
            trigger_value_divisor,
            timestamp_buffer_secs,
        })
    }

    /// Get the trigger value in wei from the tap-agent configuration
    pub fn get_tap_trigger_value_wei(&mut self) -> Result<u128> {
        let config = self.get_tap_config()?;
        Ok(config.get_trigger_value())
    }

    /// Get the timestamp buffer duration from the tap-agent configuration
    pub fn get_tap_timestamp_buffer_secs(&mut self) -> Result<u64> {
        let config = self.get_tap_config()?;
        Ok(config.timestamp_buffer_secs)
    }

    /// Get the max amount willing to lose in GRT from the tap-agent configuration
    pub fn get_tap_max_amount_willing_to_lose_grt(&mut self) -> Result<f64> {
        let config = self.get_tap_config()?;
        Ok(config.max_amount_willing_to_lose_grt)
    }

    /// Get the trigger value divisor from the tap-agent configuration
    pub fn get_tap_trigger_value_divisor(&mut self) -> Result<f64> {
        let config = self.get_tap_config()?;
        Ok(config.trigger_value_divisor)
    }
}

fn read_tap_verifier_json(path: &str) -> Result<String> {
    let p = Path::new(path);
    let bytes = fs::read(p)?;
    let parsed: TapContracts = serde_json::from_slice(&bytes)?;
    parsed
        .chain
        .and_then(|c| c.tap_verifier)
        .ok_or_else(|| anyhow::anyhow!("TAPVerifier not found in {path}"))
}

fn read_graph_tally_collector_json(path: &str) -> Result<String> {
    let p = Path::new(path);
    let bytes = fs::read(p)?;
    let parsed: HorizonJson = serde_json::from_slice(&bytes)?;
    parsed
        .chain
        .and_then(|c| c.gtc.map(|a| a.address))
        .ok_or_else(|| anyhow::anyhow!("GraphTallyCollector not found in {path}"))
}

fn read_subgraph_service_json(path: &str) -> Result<String> {
    let p = Path::new(path);
    let bytes = fs::read(p)?;
    let parsed: SubgraphServiceJson = serde_json::from_slice(&bytes)?;
    parsed
        .chain
        .and_then(|c| c.svc.map(|a| a.address))
        .ok_or_else(|| anyhow::anyhow!("SubgraphService not found in {path}"))
}

impl std::fmt::Debug for TestConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn mask(s: &str) -> String {
            if s.is_empty() {
                return String::from("<empty>");
            }
            if s.len() <= 10 {
                return String::from("********");
            }
            format!("{}…{}", &s[..6], &s[s.len().saturating_sub(4)..])
        }

        writeln!(f, "TestConfig {{")?;
        writeln!(
            f,
            "  urls: {{ indexer: {}, gateway: {}, graph: {}, metrics: {} }},",
            self.indexer_url, self.gateway_url, self.graph_url, self.tap_agent_metrics_url
        )?;
        writeln!(f, "  chain: {},", self.chain_id)?;
        writeln!(
            f,
            "  auth: {{ gateway_api_key: {} }},",
            mask(&self.gateway_api_key)
        )?;
        writeln!(
            f,
            "  accounts: {{ account0: {} (secret: {}), account1: {} (secret: {}) }},",
            self.account0_address,
            mask(&self.account0_secret),
            self.account1_address,
            mask(&self.account1_secret)
        )?;
        writeln!(
            f,
            "  contracts: {{ tap_verifier: {}, graph_tally_collector: {} }},",
            self.tap_verifier_contract, self.graph_tally_collector_contract
        )?;
        writeln!(
            f,
            "  subgraph: {{ id: {}, deployment: {} }},",
            self.subgraph_id, self.test_subgraph_deployment
        )?;
        writeln!(f, "  data_service: {}", self.test_data_service)?;
        write!(f, "}}")
    }
}
