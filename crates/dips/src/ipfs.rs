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
//! # Timeout, Retry, and Size Limits
//!
//! IPFS fetches have a 30-second timeout per attempt. On failure, the client
//! retries up to 3 times with exponential backoff (10s, 20s, 40s delays). This
//! gives IPFS meaningful recovery time between attempts.
//!
//! Worst case timing: 30s + 10s + 30s + 20s + 30s + 40s + 30s = 190 seconds.
//!
//! Dipper's gRPC timeout should be at least 220 seconds (190s + 30s buffer)
//! to avoid timing out while indexer-rs is still retrying IPFS.
//!
//! Each fetch is also capped at `IPFS_MAX_MANIFEST_BYTES`. Real manifests
//! are tens of KB; the cap exists so a caller-supplied CID cannot force
//! an unbounded download from attacker-controlled content.
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

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use derivative::Derivative;
use futures::TryStreamExt;
use ipfs_api_backend_hyper::{IpfsApi, TryFromUri};
use serde::Deserialize;

use crate::{
    inflight::{self, InflightCounter},
    DipsError,
};

/// Timeout for a single IPFS fetch attempt.
const IPFS_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of IPFS fetch attempts (1 initial + 3 retries).
const IPFS_MAX_ATTEMPTS: u32 = 4;

/// Base delay for exponential backoff between retries (10s, 20s, 40s).
const IPFS_RETRY_BASE_DELAY: Duration = Duration::from_secs(10);

/// Upper bound on bytes read from a single manifest fetch. Real manifests are
/// tens of KB; this 25 MiB cap (aligned with Graph Node's default) bounds the
/// per-request bandwidth cost of a caller-chosen CID resolving to hostile content.
pub(crate) const IPFS_MAX_MANIFEST_BYTES: usize = 25 * 1024 * 1024;

/// When the in-flight request count exceeds this threshold, IPFS fetches
/// stop retrying — a single attempt only. The fewer-retries mode frees
/// handler slots faster when the service is under load, at the cost of
/// failing proposals whose first IPFS attempt has a transient error.
pub(crate) const IPFS_DURESS_THRESHOLD: usize = 200;

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
    inflight: InflightCounter,
}

impl IpfsClient {
    pub fn new(url: &str, inflight: InflightCounter) -> anyhow::Result<Self> {
        let client = ipfs_api_backend_hyper::IpfsClient::from_str(url)?;
        Ok(Self { client, inflight })
    }

    pub(crate) fn max_attempts(&self) -> u32 {
        if inflight::snapshot(&self.inflight) > IPFS_DURESS_THRESHOLD {
            1
        } else {
            IPFS_MAX_ATTEMPTS
        }
    }
}

#[async_trait]
impl IpfsFetcher for IpfsClient {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError> {
        let mut last_error = None;
        let max_attempts = self.max_attempts();

        for attempt in 0..max_attempts {
            if attempt > 0 {
                // Exponential backoff: 10s, 20s, 40s
                let delay = IPFS_RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
                tracing::debug!(
                    file = %file,
                    attempt = attempt + 1,
                    delay_ms = delay.as_millis(),
                    "Retrying IPFS fetch after backoff"
                );
                tokio::time::sleep(delay).await;
            }

            match self.fetch_with_timeout(file).await {
                Ok(manifest) => return Ok(manifest),
                Err(e) => {
                    tracing::warn!(
                        file = %file,
                        attempt = attempt + 1,
                        max_attempts,
                        error = %e,
                        "IPFS fetch attempt failed"
                    );
                    last_error = Some(e);
                }
            }
        }

        // All attempts failed
        Err(last_error.unwrap_or_else(|| {
            DipsError::SubgraphManifestUnavailable(format!("{file}: all attempts failed"))
        }))
    }
}

impl IpfsClient {
    /// Fetch with timeout wrapper.
    async fn fetch_with_timeout(&self, file: &str) -> Result<GraphManifest, DipsError> {
        let fetch_future = async {
            let mut stream = self.client.cat(file.as_ref());
            let mut content: Vec<u8> = Vec::new();
            while let Some(chunk) = stream
                .try_next()
                .await
                .map_err(|e| DipsError::SubgraphManifestUnavailable(format!("{file}: {e}")))?
            {
                content.extend_from_slice(&chunk);
                if content.len() > IPFS_MAX_MANIFEST_BYTES {
                    return Err(DipsError::ManifestTooLarge {
                        file: file.to_string(),
                        limit_bytes: IPFS_MAX_MANIFEST_BYTES,
                    });
                }
            }

            let manifest: GraphManifest = serde_yaml::from_slice(&content)
                .map_err(|e| DipsError::InvalidSubgraphManifest(format!("{file}: {e}")))?;

            Ok(manifest)
        };

        tokio::time::timeout(IPFS_FETCH_TIMEOUT, fetch_future)
            .await
            .map_err(|_| {
                DipsError::SubgraphManifestUnavailable(format!(
                    "{file}: timeout after {}s",
                    IPFS_FETCH_TIMEOUT.as_secs()
                ))
            })?
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
    /// Creates a fetcher that returns a manifest with no network field.
    pub fn no_network() -> Self {
        Self {
            network: String::new(),
        }
    }
}

/// Test IPFS fetcher that always fails.
#[derive(Debug, Clone, Default)]
pub struct FailingIpfsFetcher;

#[async_trait]
impl IpfsFetcher for FailingIpfsFetcher {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError> {
        Err(DipsError::SubgraphManifestUnavailable(format!(
            "{file}: connection refused (test fetcher)"
        )))
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use crate::ipfs::{
        DataSource, FailingIpfsFetcher, GraphManifest, IpfsClient, IpfsFetcher, MockIpfsFetcher,
        IPFS_DURESS_THRESHOLD, IPFS_MAX_ATTEMPTS,
    };

    #[test]
    fn test_deserialize_manifest() {
        // Arrange
        let yaml = MANIFEST;

        // Act
        let manifest: GraphManifest = serde_yaml::from_str(yaml).unwrap();

        // Assert
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

    #[test]
    fn test_manifest_network_extraction() {
        // Arrange
        let manifest = GraphManifest {
            data_sources: vec![DataSource {
                network: "mainnet".to_string(),
            }],
        };

        // Act
        let network = manifest.network();

        // Assert
        assert_eq!(network, Some("mainnet"));
    }

    #[test]
    fn test_manifest_network_empty_sources() {
        // Arrange
        let manifest = GraphManifest {
            data_sources: vec![],
        };

        // Act
        let network = manifest.network();

        // Assert
        assert_eq!(network, None);
    }

    #[tokio::test]
    async fn test_mock_ipfs_fetcher_default() {
        // Arrange
        let fetcher = MockIpfsFetcher::default();

        // Act
        let manifest = fetcher.fetch("QmSomeHash").await.unwrap();

        // Assert
        assert_eq!(manifest.network(), Some("mainnet"));
    }

    #[tokio::test]
    async fn test_mock_ipfs_fetcher_custom_network() {
        // Arrange
        let fetcher = MockIpfsFetcher {
            network: "arbitrum-one".to_string(),
        };

        // Act
        let manifest = fetcher.fetch("QmSomeHash").await.unwrap();

        // Assert
        assert_eq!(manifest.network(), Some("arbitrum-one"));
    }

    #[tokio::test]
    async fn test_mock_ipfs_fetcher_no_network() {
        // Arrange
        let fetcher = MockIpfsFetcher::no_network();

        // Act
        let manifest = fetcher.fetch("QmSomeHash").await.unwrap();

        // Assert
        assert_eq!(manifest.network(), None);
    }

    #[tokio::test]
    async fn test_failing_ipfs_fetcher() {
        // Arrange
        let fetcher = FailingIpfsFetcher;

        // Act
        let result = fetcher.fetch("QmSomeHash").await;

        // Assert
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::DipsError::SubgraphManifestUnavailable(_)),
            "Expected SubgraphManifestUnavailable, got: {:?}",
            err
        );
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

    #[test]
    fn max_attempts_uses_full_budget_below_threshold() {
        // Arrange
        let inflight = Arc::new(AtomicUsize::new(0));
        let client = IpfsClient::new("http://localhost:5001", inflight.clone()).unwrap();

        // Act + Assert
        assert_eq!(client.max_attempts(), IPFS_MAX_ATTEMPTS);

        // Right at the threshold still counts as below — the check is `>`.
        inflight.store(IPFS_DURESS_THRESHOLD, Ordering::Relaxed);
        assert_eq!(client.max_attempts(), IPFS_MAX_ATTEMPTS);
    }

    #[test]
    fn max_attempts_drops_to_one_above_threshold() {
        // Arrange
        let inflight = Arc::new(AtomicUsize::new(IPFS_DURESS_THRESHOLD + 1));
        let client = IpfsClient::new("http://localhost:5001", inflight.clone()).unwrap();

        // Act + Assert
        assert_eq!(client.max_attempts(), 1);

        // And recovers when the counter falls back.
        inflight.store(0, Ordering::Relaxed);
        assert_eq!(client.max_attempts(), IPFS_MAX_ATTEMPTS);
    }
}
