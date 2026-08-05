// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! IPFS client for fetching subgraph manifests and verifying the files they link.
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
//! the chain the subgraph indexes, and every `file` link (the schema, each
//! mapping's WASM, each ABI) so those can be verified retrievable too:
//!
//! ```yaml
//! schema:
//!   file:
//!     /: /ipfs/Qm...      # <-- linked file, verified
//! dataSources:
//!   - network: mainnet    # <-- validated against supported networks
//!     mapping:
//!       file:
//!         /: /ipfs/Qm...  # <-- linked file, verified
//! ```
//!
//! Linked files are verified because graph-node resolves all of them at
//! deploy time, so a subgraph with any one missing can never be deployed,
//! and accepting its agreement leaves the indexer holding a paid obligation
//! it cannot serve. Only retrievability is checked: content is streamed,
//! counted against the size cap, and discarded.
//!
//! # Timeout, Retry, and Size Limits
//!
//! IPFS fetches have a 30-second timeout per attempt. On failure, the client
//! retries up to 3 times with exponential backoff (10s, 20s, 40s delays). This
//! gives IPFS meaningful recovery time between attempts.
//!
//! Retries are skipped when the service is busy: if more than 200 proposal
//! requests are in flight (`IPFS_DURESS_THRESHOLD`) as a fetch starts, that
//! fetch gets a single attempt and no retries. A loaded indexer frees handler
//! slots faster, at the cost of rejecting proposals whose one attempt hits a
//! transient error. Dipper can resend those.
//!
//! All IPFS work for one proposal shares `IPFS_PHASE_BUDGET`, sized to the
//! manifest fetch's own worst case while retrying: 30s + 10s + 30s + 20s +
//! 30s + 40s + 30s = 190 seconds. Linked files are verified in parallel
//! inside whatever remains, floored at one attempt's 30 seconds so a slow
//! manifest cannot starve them to zero: overall worst case 220 seconds.
//!
//! Dipper's gRPC timeout should be at least 250 seconds (220s + 30s buffer)
//! to avoid timing out while indexer-rs is still retrying IPFS.
//!
//! Every fetch, linked files included, is capped at `IPFS_MAX_MANIFEST_BYTES`
//! so a caller-supplied CID cannot force an unbounded download from
//! attacker-controlled content.
//!
//! # What This Proves
//!
//! Successfully fetching a manifest and verifying its linked files proves:
//! - The deployment ID maps to real content on IPFS
//! - The content is a valid, parseable subgraph manifest
//! - Every file the manifest links resolved on IPFS just now, so graph-node
//!   can be expected to deploy it
//!
//! What it does NOT prove:
//! - The subgraph is published on The Graph Network (GNS)
//! - The subgraph is not deprecated
//! - A grafted subgraph's base deployment is also retrievable (graft bases
//!   are not walked)
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
pub(crate) const IPFS_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling on all IPFS work for one proposal, manifest and linked files together;
/// equals the manifest fetch's own retrying worst case (30+10+30+20+30+40+30s),
/// so adding linked-file verification did not grow the documented budget.
pub(crate) const IPFS_PHASE_BUDGET: Duration = Duration::from_secs(190);

/// Maximum number of IPFS fetch attempts (1 initial + 3 retries).
const IPFS_MAX_ATTEMPTS: u32 = 4;

/// Base delay for exponential backoff between retries (10s, 20s, 40s).
const IPFS_RETRY_BASE_DELAY: Duration = Duration::from_secs(10);

/// Upper bound on bytes read from a single manifest fetch. Real manifests are
/// tens of KB; this 25 MiB cap (aligned with Graph Node's default) bounds the
/// per-request bandwidth cost of a caller-chosen CID resolving to hostile content.
pub(crate) const IPFS_MAX_MANIFEST_BYTES: usize = 25 * 1024 * 1024;

/// When the in-flight request count exceeds this threshold, IPFS fetches get a
/// single attempt and no retries, freeing handler slots faster under load at
/// the cost of failing proposals whose one attempt hits a transient error.
pub(crate) const IPFS_DURESS_THRESHOLD: usize = 200;

#[async_trait]
pub trait IpfsFetcher: Send + Sync + std::fmt::Debug {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError>;

    /// Prove a manifest-linked file is retrievable; the content is discarded.
    async fn verify_file(&self, file: &str) -> Result<(), DipsError>;
}

#[async_trait]
impl<T: IpfsFetcher> IpfsFetcher for Arc<T> {
    async fn fetch(&self, file: &str) -> Result<GraphManifest, DipsError> {
        self.as_ref().fetch(file).await
    }

    async fn verify_file(&self, file: &str) -> Result<(), DipsError> {
        self.as_ref().verify_file(file).await
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
        self.with_retries(file, || self.fetch_with_timeout(file))
            .await
    }

    async fn verify_file(&self, file: &str) -> Result<(), DipsError> {
        self.with_retries(file, || self.verify_with_timeout(file))
            .await
    }
}

impl IpfsClient {
    /// Run one IPFS operation under the shared retry policy: up to
    /// `max_attempts()` tries with 10s/20s/40s backoff between them.
    async fn with_retries<T, F, Fut>(&self, file: &str, op: F) -> Result<T, DipsError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, DipsError>>,
    {
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

            match op().await {
                Ok(value) => return Ok(value),
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

    /// Stream a linked file to prove it is retrievable, counting bytes against
    /// the cap and discarding them so peak memory is one chunk, not the file.
    async fn verify_with_timeout(&self, file: &str) -> Result<(), DipsError> {
        let verify_future = async {
            let mut stream = self.client.cat(file.as_ref());
            let mut total: usize = 0;
            while let Some(chunk) = stream
                .try_next()
                .await
                .map_err(|e| DipsError::SubgraphManifestUnavailable(format!("{file}: {e}")))?
            {
                total += chunk.len();
                if total > IPFS_MAX_MANIFEST_BYTES {
                    return Err(DipsError::ManifestTooLarge {
                        file: file.to_string(),
                        limit_bytes: IPFS_MAX_MANIFEST_BYTES,
                    });
                }
            }
            Ok(())
        };

        tokio::time::timeout(IPFS_FETCH_TIMEOUT, verify_future)
            .await
            .map_err(|_| {
                DipsError::SubgraphManifestUnavailable(format!(
                    "{file}: timeout after {}s",
                    IPFS_FETCH_TIMEOUT.as_secs()
                ))
            })?
    }
}

/// An IPFS link in a manifest: `file: { "/": "/ipfs/Qm..." }`.
#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct FileLink {
    #[serde(rename = "/", default)]
    path: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbiRef {
    #[serde(default)]
    file: Option<FileLink>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mapping {
    #[serde(default)]
    file: Option<FileLink>,
    #[serde(default)]
    abis: Vec<AbiRef>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaRef {
    #[serde(default)]
    file: Option<FileLink>,
}

/// A template data source; unlike `DataSource` its network may be absent.
#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    #[serde(default)]
    mapping: Option<Mapping>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSource {
    network: String,
    #[serde(default)]
    mapping: Option<Mapping>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphManifest {
    #[serde(default)]
    schema: Option<SchemaRef>,
    data_sources: Vec<DataSource>,
    #[serde(default)]
    templates: Vec<Template>,
}

impl GraphManifest {
    pub fn network(&self) -> Option<&str> {
        self.data_sources.first().map(|ds| ds.network.as_str())
    }

    /// Every file the manifest links (schema, mapping WASM, ABIs) as bare CIDs,
    /// deduplicated. graph-node resolves all of these at deploy time, so each
    /// must be retrievable for the subgraph to be deployable at all.
    pub fn linked_files(&self) -> Vec<&str> {
        let mappings = self
            .data_sources
            .iter()
            .filter_map(|ds| ds.mapping.as_ref())
            .chain(self.templates.iter().filter_map(|t| t.mapping.as_ref()));

        let schema_link = self.schema.iter().filter_map(|s| s.file.as_ref());
        let mapping_links = mappings.clone().filter_map(|m| m.file.as_ref());
        let abi_links = mappings
            .flat_map(|m| m.abis.iter())
            .filter_map(|a| a.file.as_ref());

        let mut seen: Vec<&str> = Vec::new();
        for link in schema_link.chain(mapping_links).chain(abi_links) {
            let cid = link.path.strip_prefix("/ipfs/").unwrap_or(&link.path);
            if !cid.is_empty() && !seen.contains(&cid) {
                seen.push(cid);
            }
        }
        seen
    }
}

/// Mock IPFS fetcher for testing with configurable network, manifest links,
/// and files whose verification should fail as if gone from IPFS.
#[derive(Debug, Clone)]
pub struct MockIpfsFetcher {
    pub network: String,
    /// CIDs embedded in the returned manifest as ABI links.
    pub linked_files: Vec<String>,
    /// CIDs whose `verify_file` fails, as content gone from IPFS would.
    pub missing_files: Vec<String>,
}

impl MockIpfsFetcher {
    /// Creates a fetcher that returns a manifest with no network field.
    pub fn no_network() -> Self {
        Self {
            network: String::new(),
            ..Default::default()
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

    async fn verify_file(&self, file: &str) -> Result<(), DipsError> {
        Err(DipsError::SubgraphManifestUnavailable(format!(
            "{file}: connection refused (test fetcher)"
        )))
    }
}

/// Test IPFS fetcher returning a manifest whose single data source has an
/// empty network field, to exercise the malformed-manifest path.
#[derive(Debug, Clone, Default)]
pub struct EmptyNetworkIpfsFetcher;

#[async_trait]
impl IpfsFetcher for EmptyNetworkIpfsFetcher {
    async fn fetch(&self, _file: &str) -> Result<GraphManifest, DipsError> {
        Ok(GraphManifest {
            data_sources: vec![DataSource {
                network: String::new(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }

    async fn verify_file(&self, _file: &str) -> Result<(), DipsError> {
        Ok(())
    }
}

impl Default for MockIpfsFetcher {
    fn default() -> Self {
        Self {
            network: "mainnet".to_string(),
            linked_files: vec![],
            missing_files: vec![],
        }
    }
}

#[async_trait]
impl IpfsFetcher for MockIpfsFetcher {
    async fn fetch(&self, _file: &str) -> Result<GraphManifest, DipsError> {
        if self.network.is_empty() {
            return Ok(GraphManifest::default());
        }
        Ok(GraphManifest {
            data_sources: vec![DataSource {
                network: self.network.clone(),
                mapping: Some(Mapping {
                    file: None,
                    abis: self
                        .linked_files
                        .iter()
                        .map(|f| AbiRef {
                            file: Some(FileLink { path: f.clone() }),
                        })
                        .collect(),
                }),
            }],
            ..Default::default()
        })
    }

    async fn verify_file(&self, file: &str) -> Result<(), DipsError> {
        if self.missing_files.iter().any(|f| f == file) {
            return Err(DipsError::SubgraphManifestUnavailable(format!(
                "{file}: not found (mock)"
            )));
        }
        Ok(())
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
        assert_eq!(manifest.network(), Some("scroll"));
        assert_eq!(manifest.data_sources.len(), 2);
        assert_eq!(manifest.templates.len(), 1);
    }

    #[test]
    fn test_linked_files_extraction() {
        // Arrange
        let manifest: GraphManifest = serde_yaml::from_str(MANIFEST).unwrap();

        // Act
        let files = manifest.linked_files();

        // Assert: schema first, then mapping files (data sources, then
        // templates), then ABI files in the same order; the Factory ABI
        // appears twice in the manifest and must be deduplicated.
        assert_eq!(
            files,
            vec![
                "QmSCM39NPLAjNQXsnkqq6H8z8KBi5YkfYyApPYLQbbC2kb",
                "Qmbj3ituUaFRnTuahJ8yCG9GPiPqsRYq2T7umucZzPpLFn",
                "QmcWrYawVufpST4u2Ed8Jz6jxFFaYXxERGwqstrpniY8C5",
                "QmPtcuzBcWWBGXFKGdfUgqZLJov4c4Crt85ANbER2eHdCb",
                "QmTU8eKx6pCgtff6Uvc7srAwR8BPiM3jTMBw9ahrXBjRzY",
                "QmaxxqQ7xzbGDPWu184uoq2g5sofazB9B9tEDrpPjmRZ8q",
                "QmULRc8Ac1J6YFy11z7JRpyThb6f7nmL5mMTQvN7LKj2Vy",
                "QmXuTbDkNrN27VydxbS2huvKRk62PMgUTdPDWkxcr2w7j2",
            ]
        );
    }

    #[test]
    fn test_linked_files_empty_manifest() {
        // Arrange: no schema, no mappings, nothing linked.
        let manifest = GraphManifest::default();

        // Act + Assert
        assert!(manifest.linked_files().is_empty());
    }

    #[tokio::test]
    async fn test_mock_fetcher_embeds_links_and_fails_missing_files() {
        // Arrange
        let fetcher = MockIpfsFetcher {
            linked_files: vec!["QmPresent".to_string(), "QmGone".to_string()],
            missing_files: vec!["QmGone".to_string()],
            ..Default::default()
        };

        // Act
        let manifest = fetcher.fetch("QmSomeHash").await.unwrap();

        // Assert
        assert_eq!(manifest.linked_files(), vec!["QmPresent", "QmGone"]);
        assert!(fetcher.verify_file("QmPresent").await.is_ok());
        assert!(matches!(
            fetcher.verify_file("QmGone").await,
            Err(crate::DipsError::SubgraphManifestUnavailable(_))
        ));
    }

    #[test]
    fn test_manifest_network_extraction() {
        // Arrange
        let manifest = GraphManifest {
            data_sources: vec![DataSource {
                network: "mainnet".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        // Act
        let network = manifest.network();

        // Assert
        assert_eq!(network, Some("mainnet"));
    }

    #[test]
    fn test_manifest_network_empty_sources() {
        // Arrange
        let manifest = GraphManifest::default();

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
            ..Default::default()
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

        // Right at the threshold still counts as below, because the check is `>`.
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
