// Copyright 2025-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{env, fs, path::Path};

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

#[derive(Clone)]
pub struct TestConfig {
    pub indexer_url: String,
    pub gateway_url: String,
    pub graph_url: String,
    pub tap_agent_metrics_url: String,

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
        })
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
