// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! IPFS client for fetching subgraph manifests.
//!
//! When validating an RCA, we need to verify that the referenced subgraph
//! deployment actually exists and determine which network it indexes.
//! The subgraph deployment ID in the RCA is a bytes32 that maps to an IPFS
//! CIDv0 hash pointing to the subgraph manifest.
//!
//! # Manifest Structure
//!
//! Subgraph manifests are YAML files containing data source definitions.
//! We extract the `network` field to validate that this indexer supports
//! the chain the subgraph indexes:
//!
//! ```yaml
//! dataSources:
//!   - network: mainnet    # <-- This is what we extract
//!     kind: ethereum/contract
//!     ...
//! ```
//!
//! # What This Proves
//!
//! Successfully fetching a manifest proves:
//! - The deployment ID maps to real content on IPFS
//! - The content is a valid, parseable subgraph manifest
//!
//! What it does NOT prove:
//! - The subgraph is published on The Graph Network (GNS)
//! - The subgraph is not deprecated
//!
//! Those checks are the indexer-agent's responsibility.

use std::sync::Arc;

use async_trait::async_trait;
use derivative::Derivative;
use futures::TryStreamExt;
use ipfs_api_backend_hyper::{IpfsApi, TryFromUri};
use serde::Deserialize;

use crate::DipsError;

#[async_trait]
pub trait IpfsFetcher: Send + Sync + std::fmt::Debug {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError>;
}

#[async_trait]
impl<T: IpfsFetcher> IpfsFetcher for Arc<T> {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError> {
        self.as_ref().fetch(file).await
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct IpfsClient {
    #[derivative(Debug = "ignore")]
    client: ipfs_api_backend_hyper::IpfsClient,
}

impl IpfsClient {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let client = ipfs_api_backend_hyper::IpfsClient::from_str(url)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl IpfsFetcher for IpfsClient {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError> {
        let content = self
            .client
            .cat(file.as_ref())
            .map_ok(|chunk| chunk.to_vec())
            .try_concat()
            .await
            .map_err(|e| {
                tracing::warn!("Failed to fetch subgraph manifest {}: {}", file, e);
                DipsError::SubgraphManifestUnavailable(format!("{file}: {e}"))
            })?;

        let manifest: GraphManifest = serde_yaml::from_slice(&content).map_err(|e| {
            tracing::warn!("Failed to parse subgraph manifest {}: {}", file, e);
            DipsError::InvalidSubgraphManifest(format!("{file}: {e}"))
        })?;

        Ok(manifest)
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSource {
    network: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphManifest {
    data_sources: Vec<DataSource>,
}

impl GraphManifest {
    pub fn network(&self) -> Option<&str> {
        self.data_sources.first().map(|ds| ds.network.as_str())
    }
}

/// Mock IPFS fetcher for testing with configurable network.
#[derive(Debug, Clone)]
pub struct MockIpfsFetcher {
    pub network: String,
}

impl MockIpfsFetcher {
    pub fn no_network() -> Self {
        Self {
            network: String::new(),
        }
    }
}

impl Default for MockIpfsFetcher {
    fn default() -> Self {
        Self {
            network: "mainnet".to_string(),
        }
    }
}

#[async_trait]
impl IpfsFetcher for MockIpfsFetcher {
    async fn fetch(&self, _file: &str) -> Result<GraphManifest, DipsError> {
        if self.network.is_empty() {
            Ok(GraphManifest {
                data_sources: vec![],
            })
        } else {
            Ok(GraphManifest {
                data_sources: vec![DataSource {
                    network: self.network.clone(),
                }],
            })
        }
    }
}

#[cfg(test)]
mod test {
    use crate::ipfs::{DataSource, GraphManifest};

    #[test]
    fn test_deserialize_manifest() {
        let manifest: GraphManifest = serde_yaml::from_str(MANIFEST).unwrap();
        assert_eq!(
            manifest,
            GraphManifest {
                data_sources: vec![
                    DataSource {
                        network: "scroll".to_string()
                    },
                    DataSource {
                        network: "scroll".to_string()
                    }
                ],
            }
        )
    }

    const MANIFEST: &str = "
dataSources:
  - kind: ethereum/contract
    mapping:
      abis:
        - file:
            /: /ipfs/QmTU8eKx6pCgtff6Uvc7srAwR8BPiM3jTMBw9ahrXBjRzY
          name: Factory
      apiVersion: 0.0.6
      entities: []
      eventHandlers:
        - event: >-
            PoolCreated(indexed address,indexed address,indexed
            uint24,int24,address)
          handler: handlePoolCreated
      file:
        /: /ipfs/Qmbj3ituUaFRnTuahJ8yCG9GPiPqsRYq2T7umucZzPpLFn
      kind: ethereum/events
      language: wasm/assemblyscript
    name: Factory
    network: scroll
    source:
      abi: Factory
      address: '0x46B3fDF7b5CDe91Ac049936bF0bDb12c5d22202e'
      startBlock: 82522
  - kind: ethereum/contract
    mapping:
      abis:
        - file:
            /: /ipfs/QmaxxqQ7xzbGDPWu184uoq2g5sofazB9B9tEDrpPjmRZ8q
          name: NonfungiblePositionManager

      apiVersion: 0.0.6
      entities: []
      eventHandlers:
        - event: 'IncreaseLiquidity(indexed uint256,uint128,uint256,uint256)'
          handler: handleIncreaseLiquidity
      file:
        /: /ipfs/QmcWrYawVufpST4u2Ed8Jz6jxFFaYXxERGwqstrpniY8C5
      kind: ethereum/events
      language: wasm/assemblyscript
    name: NonfungiblePositionManager
    network: scroll
    source:
      abi: NonfungiblePositionManager
      address: '0x0389879e0156033202C44BF784ac18fC02edeE4f'
      startBlock: 82597
features:
  - nonFatalErrors
schema:
  file:
    /: /ipfs/QmSCM39NPLAjNQXsnkqq6H8z8KBi5YkfYyApPYLQbbC2kb
specVersion: 0.0.4
templates:
  - kind: ethereum/contract
    mapping:
      abis:
        - file:
            /: /ipfs/QmULRc8Ac1J6YFy11z7JRpyThb6f7nmL5mMTQvN7LKj2Vy
          name: Pool
        - file:
            /: /ipfs/QmTU8eKx6pCgtff6Uvc7srAwR8BPiM3jTMBw9ahrXBjRzY
          name: Factory
        - file:
            /: /ipfs/QmXuTbDkNrN27VydxbS2huvKRk62PMgUTdPDWkxcr2w7j2
          name: ERC20
      apiVersion: 0.0.6
      entities: []
      eventHandlers:
        - event: 'Initialize(uint160,int24)'
          handler: handleInitialize
        - event: >-
            Swap(indexed address,indexed
            address,int256,int256,uint160,uint128,int24)
          handler: handleSwap
        - event: >-
            Mint(address,indexed address,indexed int24,indexed
            int24,uint128,uint256,uint256)
          handler: handleMint
        - event: >-
            Burn(indexed address,indexed int24,indexed
            int24,uint128,uint256,uint256)
          handler: handleBurn
        - event: >-
            Flash(indexed address,indexed
            address,uint256,uint256,uint256,uint256)
          handler: handleFlash
        - event: >-
            Collect(indexed address,address,indexed int24,indexed
            int24,uint128,uint128)
          handler: handlePoolCollect
        - event: 'CollectProtocol(indexed address,indexed address,uint128,uint128)'
          handler: handleProtocolCollect
        - event: 'SetFeeProtocol(uint8,uint8,uint8,uint8)'
          handler: handleSetProtocolFee
      file:
        /: /ipfs/QmPtcuzBcWWBGXFKGdfUgqZLJov4c4Crt85ANbER2eHdCb
      kind: ethereum/events
      language: wasm/assemblyscript
    name: Pool
    network: scroll
    source:
      abi: Pool

    ";
}
