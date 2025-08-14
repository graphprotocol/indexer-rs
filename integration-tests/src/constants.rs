// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
//
//
//! Constants used in the integration tests for the TAP RAV generation
//! their value is taken from local-network .env variable

pub const INDEXER_URL: &str = "http://localhost:7601";

pub const GATEWAY_API_KEY: &str = "deadbeefdeadbeefdeadbeefdeadbeef";
pub const GATEWAY_URL: &str = "http://localhost:7700";
pub const TAP_AGENT_METRICS_URL: &str = "http://localhost:7300/metrics";

// The deployed gateway and indexer
// use this verifier contract
// which must be part of the eip712 domain
// and the signing key account0_secret
// they must match otherwise receipts would be rejected
pub const TAP_VERIFIER_CONTRACT: &str = "0xC9a43158891282A2B1475592D5719c001986Aaec";

// V2 GraphTallyCollector contract address (for Horizon receipts)
pub const GRAPH_TALLY_COLLECTOR_CONTRACT: &str = "0xB0D4afd8879eD9F52b28595d31B441D079B2Ca07";
pub const ACCOUNT0_SECRET: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const CHAIN_ID: u64 = 1337;

pub const SUBGRAPH_ID: &str = "Qmc2CbqucMvaS4GFvt2QUZWvRwSZ3K5ipeGvbC6UUBf616";

pub const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

pub const GRT_DECIMALS: u8 = 18;
pub const GRT_BASE: u128 = 10u128.pow(GRT_DECIMALS as u32);

pub const MAX_RECEIPT_VALUE: u128 = GRT_BASE / 10_000;

// Data service address for V2 testing
// For testing, we'll use the indexer address as the data service
pub const TEST_DATA_SERVICE: &str = "0xf4ef6650e48d099a4972ea5b414dab86e1998bd3"; // indexer address
