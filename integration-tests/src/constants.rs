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
pub const TAP_VERIFIER_CONTRACT: &str = "0x8198f5d8F8CfFE8f9C413d98a0A55aEB8ab9FbB7";
pub const ACCOUNT0_SECRET: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const CHAIN_ID: u64 = 1337;

pub const SUBGRAPH_ID: &str = "QmV4R5g7Go94bVFmKTVFG7vaMTb1ztUUWb45mNrsc7Yyqs";

pub const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

pub const GRT_DECIMALS: u8 = 18;
pub const GRT_BASE: u128 = 10u128.pow(GRT_DECIMALS as u32);

pub const MAX_RECEIPT_VALUE: u128 = GRT_BASE / 10_000;
