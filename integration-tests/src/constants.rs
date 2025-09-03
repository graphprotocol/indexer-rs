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

// Wallet
// - Account 0 used by: EBO, admin actions (deploy contracts, transfer ETH/GRT), gateway sender for PaymentsEscrow
// - Account 1 used by: Gateway signer for PaymentsEscrow
// pub const MNEMONIC: &str = "test test test test test test test test test test test junk";
// pub const ACCOUNT0_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
pub const ACCOUNT0_SECRET: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
// pub const ACCOUNT1_ADDRESS: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
// pub const ACCOUNT1_SECRET: &str =
// "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

// The deployed gateway and indexer
// use this verifier contract
// which must be part of the eip712 domain
// and the signing key account0_secret
// they must match otherwise receipts would be rejected
pub const TAP_VERIFIER_CONTRACT: &str = "0xC9a43158891282A2B1475592D5719c001986Aaec";

// pub const V1_DOMAIN_NAME: &str = "TAP";
// pub const V2_DOMAIN_NAME: &str = "GraphTally";

// V2 GraphTallyCollector contract address (for Horizon receipts)
pub const GRAPH_TALLY_COLLECTOR_CONTRACT: &str = "0xB0D4afd8879eD9F52b28595d31B441D079B2Ca07";
pub const CHAIN_ID: u64 = 1337;

pub const SUBGRAPH_ID: &str = "Qmc2CbqucMvaS4GFvt2QUZWvRwSZ3K5ipeGvbC6UUBf616";
pub const TEST_SUBGRAPH_DEPLOYMENT: &str = "QmRcucmbxAXLaAZkkCR8Bdj1X7QGPLjfRmQ5H6tFhGqiHX";

pub const GRAPH_URL: &str = "http://localhost:8000/subgraphs/name/graph-network";

pub const GRT_DECIMALS: u8 = 18;
pub const GRT_BASE: u128 = 10u128.pow(GRT_DECIMALS as u32);

pub const MAX_RECEIPT_VALUE: u128 = GRT_BASE / 10_000;

// Data service address for V2 testing
// For testing, we'll use the indexer address as the data service
pub const TEST_DATA_SERVICE: &str = "0xf4ef6650e48d099a4972ea5b414dab86e1998bd3"; // indexer address

// Gateway mnemonic for direct service testing (from local-network-semiotic/.env)
// pub const GATEWAY_MNEMONIC: &str = "test test test test test test test test test test test junk";

// Gateway uses ACCOUNT0_SECRET as raw private key (not mnemonic)
// pub const GATEWAY_PRIVATE_KEY: &str =
//     "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

// Kafka configuration (from local-network-semiotic/.env)
pub const KAFKA_SERVERS: &str = "localhost:9092";

// Taken from indexer-service configuration
// pub const POSTGRES_PORT: &str = "5432";
pub const POSTGRES_URL: &str = "postgresql://postgres@localhost:5432/indexer_components_1";
