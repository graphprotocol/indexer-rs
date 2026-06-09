// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Trust gate for incoming agreement proposals: require the recovered EIP-712
//! signer to hold the on-chain `AGREEMENT_MANAGER_ROLE` (read from the
//! indexing-payments-subgraph), gating on the signer, never the spoofable payer.

use async_trait::async_trait;
use thegraph_core::alloy::primitives::Address;

use crate::DipsError;

/// Source of truth for which signers may send agreement proposals.
#[async_trait]
pub trait TrustedSignerSource: std::fmt::Debug + Send + Sync {
    /// Confirm `signer` may send agreement proposals. `Ok` = trusted;
    /// `SenderNotTrusted` = definitively not a role holder; any other error means
    /// the check failed and the caller should treat it as transient and retry.
    async fn verify_trusted(&self, signer: Address) -> Result<(), DipsError>;
}

/// A fixed set of trusted signers. Used in tests and as a simple in-memory
/// source; production uses [`SubgraphTrustedSigners`].
#[derive(Debug, Clone, Default)]
pub struct StaticTrustedSigners(pub std::collections::HashSet<Address>);

#[async_trait]
impl TrustedSignerSource for StaticTrustedSigners {
    async fn verify_trusted(&self, signer: Address) -> Result<(), DipsError> {
        if self.0.contains(&signer) {
            Ok(())
        } else {
            Err(DipsError::SenderNotTrusted { signer })
        }
    }
}

#[cfg(feature = "db")]
pub use subgraph::SubgraphTrustedSigners;

#[cfg(feature = "db")]
mod subgraph {
    use std::{
        collections::HashSet,
        sync::{Arc, RwLock},
        time::Duration,
    };

    use async_trait::async_trait;
    use indexer_monitor::SubgraphClient;
    use thegraph_core::alloy::primitives::Address;
    use tokio::{sync::Mutex, time::Instant};

    use super::TrustedSignerSource;
    use crate::DipsError;

    /// Fetches every active AGREEMENT_MANAGER_ROLE holder in one query, plus the
    /// subgraph head time for the staleness guard. The role filter and `active:
    /// true` are load-bearing: several roles are indexed, and revokes only flag.
    const ROLE_HOLDERS_QUERY: &str = r#"{"query":"{ roleAssignments(where: {role: \"0xeb1b3455811b30c0dd237887f6349f22cc96ee5963709d7fe356d9b0cefa6d22\", active: true}, first: 1000) { account } _meta { block { timestamp } } }"}"#;

    /// Page size for the holder query. The manager set is tiny in practice; if a
    /// fetch ever returns a full page we log it rather than silently truncating.
    const MAX_ROLE_HOLDERS: usize = 1000;

    /// After a failed on-demand fetch, don't re-hit the subgraph again for this
    /// long, so a burst of unknown signers can't turn into a fetch storm.
    const REFRESH_DEBOUNCE: Duration = Duration::from_secs(5);

    #[derive(Default)]
    struct RoleCache {
        holders: HashSet<Address>,
        /// When the holder set was last fetched successfully.
        last_success: Option<Instant>,
        /// When a fetch most recently failed (for debouncing retries).
        last_failed_attempt: Option<Instant>,
    }

    /// Caches the AGREEMENT_MANAGER_ROLE holder set, refreshing on a timer and on
    /// demand for unknown signers. A cache hit within the bounded window is
    /// answered from memory (fail-open through outages); past it, fails closed.
    pub struct SubgraphTrustedSigners {
        subgraph: &'static SubgraphClient,
        cache: Arc<RwLock<RoleCache>>,
        refresh_lock: Arc<Mutex<()>>,
        /// Oldest a successful fetch may be before a cache hit stops being
        /// trusted: the bounded fail-open window. Set to the refresh interval
        /// plus a grace period so a healthy deploy never trips it.
        max_stale: Duration,
        /// Largest gap between the subgraph head and wall-clock that is still
        /// trusted; past it the data is treated as unreliable. 0 disables it.
        max_chain_lag: Duration,
    }

    impl std::fmt::Debug for SubgraphTrustedSigners {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SubgraphTrustedSigners")
                .field("max_stale", &self.max_stale)
                .field("max_chain_lag", &self.max_chain_lag)
                .finish_non_exhaustive()
        }
    }

    impl SubgraphTrustedSigners {
        /// Construct the source without fetching or starting the refresh task.
        fn new(
            subgraph: &'static SubgraphClient,
            max_stale: Duration,
            max_chain_lag: Duration,
        ) -> Arc<Self> {
            Arc::new(Self {
                subgraph,
                cache: Arc::new(RwLock::new(RoleCache::default())),
                refresh_lock: Arc::new(Mutex::new(())),
                max_stale,
                max_chain_lag,
            })
        }

        /// Build the source, seed it once (best-effort), and start the periodic
        /// refresh. A failed initial fetch is logged, not fatal: proposals are
        /// rejected as transient until the first successful fetch.
        pub async fn spawn(
            subgraph: &'static SubgraphClient,
            refresh_interval: Duration,
            failopen_grace: Duration,
            max_chain_lag: Duration,
        ) -> Arc<Self> {
            let this = Self::new(subgraph, refresh_interval + failopen_grace, max_chain_lag);

            if let Err(e) = this.refresh().await {
                tracing::warn!(
                    error = %e,
                    "initial agreement-manager role fetch failed; DIPs proposals \
                     will be rejected as transient until it succeeds"
                );
            }

            let bg = this.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(refresh_interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                tick.tick().await; // the immediate first tick; already seeded above
                loop {
                    tick.tick().await;
                    if let Err(e) = bg.refresh().await {
                        tracing::warn!(error = %e, "periodic agreement-manager role refresh failed");
                    }
                }
            });

            this
        }

        fn is_fresh(&self, last_success: Option<Instant>) -> bool {
            last_success.is_some_and(|t| t.elapsed() < self.max_stale)
        }

        /// Fetch the whole holder set and swap it into the cache. Single-flighted
        /// (concurrent callers coalesce onto one fetch) and debounced after a
        /// failure so an outage doesn't trigger a fetch per request.
        async fn refresh(&self) -> Result<(), DipsError> {
            let started = Instant::now();
            let _guard = self.refresh_lock.lock().await;

            {
                let cache = self.cache.read().unwrap();
                // A concurrent refresh already succeeded after we started waiting.
                if cache.last_success.is_some_and(|t| t >= started) {
                    return Ok(());
                }
                // We failed very recently; don't hammer the subgraph.
                if cache
                    .last_failed_attempt
                    .is_some_and(|t| t.elapsed() < REFRESH_DEBOUNCE)
                {
                    return Err(DipsError::TrustVerificationUnavailable(
                        "agreement-manager role subgraph was unreachable moments ago".to_string(),
                    ));
                }
            }

            match self.fetch_holders().await {
                Ok(holders) => {
                    let mut cache = self.cache.write().unwrap();
                    cache.holders = holders;
                    cache.last_success = Some(Instant::now());
                    cache.last_failed_attempt = None;
                    Ok(())
                }
                Err(e) => {
                    self.cache.write().unwrap().last_failed_attempt = Some(Instant::now());
                    Err(DipsError::TrustVerificationUnavailable(e.to_string()))
                }
            }
        }

        async fn fetch_holders(&self) -> anyhow::Result<HashSet<Address>> {
            let body = bytes::Bytes::from_static(ROLE_HOLDERS_QUERY.as_bytes());
            let response = self
                .subgraph
                .query_raw(body)
                .await
                .map_err(|e| anyhow::anyhow!("role-holder query failed: {e}"))?;
            let parsed: GraphqlResponse = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("decoding role-holder response failed: {e}"))?;

            if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
                let joined = errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(anyhow::anyhow!(
                    "role-holder query returned errors: {joined}"
                ));
            }

            let data = parsed
                .data
                .ok_or_else(|| anyhow::anyhow!("role-holder response had no data"))?;

            // A badly-lagging subgraph may be missing recent grants or revokes,
            // so treat it as unreliable rather than trusting a stale role set.
            let max_lag = self.max_chain_lag.as_secs() as i64;
            if max_lag > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("system time before unix epoch: {e}"))?
                    .as_secs() as i64;
                let lag = now - data.meta.block.timestamp;
                if lag > max_lag {
                    return Err(anyhow::anyhow!(
                        "indexing-payments subgraph is {lag}s behind wall-clock (max {max_lag}s); \
                         treating agreement-manager role data as unreliable"
                    ));
                }
            }

            if data.role_assignments.len() >= MAX_ROLE_HOLDERS {
                tracing::warn!(
                    returned = data.role_assignments.len(),
                    "agreement-manager role query hit the page cap; some holders may be missing"
                );
            }

            let holders: HashSet<Address> = data
                .role_assignments
                .into_iter()
                .map(|r| r.account)
                .collect();
            if holders.is_empty() {
                tracing::warn!(
                    "agreement-manager role set is empty; all DIPs proposals will be \
                     rejected as untrusted"
                );
            }
            Ok(holders)
        }
    }

    #[async_trait]
    impl TrustedSignerSource for SubgraphTrustedSigners {
        async fn verify_trusted(&self, signer: Address) -> Result<(), DipsError> {
            // Fast path: a known holder whose cache is still inside the window is
            // answered from memory, with no subgraph round-trip.
            {
                let cache = self.cache.read().unwrap();
                if cache.holders.contains(&signer) && self.is_fresh(cache.last_success) {
                    return Ok(());
                }
            }

            // Unknown signer, or the cache has gone stale past the window: try a
            // single-flighted refresh, then decide against the freshest data.
            let refreshed = self.refresh().await;

            let (present, fresh) = {
                let cache = self.cache.read().unwrap();
                (
                    cache.holders.contains(&signer),
                    self.is_fresh(cache.last_success),
                )
            };

            match refreshed {
                // Authoritative answer from a fresh fetch.
                Ok(()) if present => Ok(()),
                Ok(()) => Err(DipsError::SenderNotTrusted { signer }),
                // Couldn't refresh: fail open only for a known holder still inside
                // the window (e.g. a concurrent refresh just succeeded); otherwise
                // we can't vouch for the signer, so report transient.
                Err(transient) => {
                    if present && fresh {
                        tracing::warn!(
                            %signer,
                            "agreement-manager role refresh failed; admitting known \
                             signer from cached set within fail-open window"
                        );
                        Ok(())
                    } else {
                        Err(transient)
                    }
                }
            }
        }
    }

    #[derive(serde::Deserialize)]
    struct GraphqlResponse {
        data: Option<RoleData>,
        #[serde(default)]
        errors: Option<Vec<GraphqlError>>,
    }

    #[derive(serde::Deserialize)]
    struct GraphqlError {
        message: String,
    }

    #[derive(serde::Deserialize)]
    struct RoleData {
        #[serde(rename = "roleAssignments")]
        role_assignments: Vec<RoleRow>,
        #[serde(rename = "_meta")]
        meta: Meta,
    }

    #[derive(serde::Deserialize)]
    struct RoleRow {
        account: Address,
    }

    #[derive(serde::Deserialize)]
    struct Meta {
        block: MetaBlock,
    }

    #[derive(serde::Deserialize)]
    struct MetaBlock {
        timestamp: i64,
    }

    #[cfg(test)]
    mod test {
        use std::time::Duration;

        use indexer_monitor::{DeploymentDetails, SubgraphClient};
        use serde_json::json;
        use thegraph_core::alloy::primitives::{address, Address};
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        use super::*;

        const HOLDER: Address = address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        const STRANGER: Address = address!("0000000000000000000000000000000000000099");

        fn now_secs() -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        }

        fn holders_body(accounts: &[Address], timestamp: i64) -> serde_json::Value {
            json!({
                "data": {
                    "roleAssignments": accounts
                        .iter()
                        .map(|a| json!({ "account": a }))
                        .collect::<Vec<_>>(),
                    "_meta": { "block": { "timestamp": timestamp } }
                }
            })
        }

        async fn leak_client(server: &MockServer) -> &'static SubgraphClient {
            Box::leak(Box::new(
                SubgraphClient::new(
                    reqwest::Client::new(),
                    None,
                    DeploymentDetails::for_query_url(&server.uri()).unwrap(),
                )
                .await,
            ))
        }

        async fn client_always(
            template: ResponseTemplate,
        ) -> (&'static SubgraphClient, MockServer) {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(template)
                .mount(&server)
                .await;
            let client = leak_client(&server).await;
            (client, server)
        }

        #[tokio::test]
        async fn known_signer_passes() {
            let (client, _server) = client_always(
                ResponseTemplate::new(200).set_body_json(holders_body(&[HOLDER], now_secs())),
            )
            .await;
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);
            src.refresh().await.unwrap();

            assert!(src.verify_trusted(HOLDER).await.is_ok());
        }

        #[tokio::test]
        async fn unknown_signer_rejected_after_refresh() {
            let (client, _server) = client_always(
                ResponseTemplate::new(200).set_body_json(holders_body(&[HOLDER], now_secs())),
            )
            .await;
            // Cold cache: the verify triggers an on-demand refresh, then rejects.
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);

            assert!(matches!(
                src.verify_trusted(STRANGER).await,
                Err(DipsError::SenderNotTrusted { .. })
            ));
        }

        #[tokio::test]
        async fn empty_role_set_rejects() {
            let (client, _server) = client_always(
                ResponseTemplate::new(200).set_body_json(holders_body(&[], now_secs())),
            )
            .await;
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);

            assert!(matches!(
                src.verify_trusted(HOLDER).await,
                Err(DipsError::SenderNotTrusted { .. })
            ));
        }

        #[tokio::test]
        async fn unreachable_subgraph_is_transient() {
            let (client, _server) = client_always(ResponseTemplate::new(500)).await;
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);

            assert!(matches!(
                src.verify_trusted(HOLDER).await,
                Err(DipsError::TrustVerificationUnavailable(_))
            ));
        }

        #[tokio::test]
        async fn stale_subgraph_head_is_transient() {
            // Head far behind wall-clock with a 60s tolerance: treated as unreliable.
            let (client, _server) =
                client_always(ResponseTemplate::new(200).set_body_json(holders_body(&[HOLDER], 1)))
                    .await;
            let src = SubgraphTrustedSigners::new(
                client,
                Duration::from_secs(60),
                Duration::from_secs(60),
            );

            assert!(matches!(
                src.verify_trusted(HOLDER).await,
                Err(DipsError::TrustVerificationUnavailable(_))
            ));
        }

        #[tokio::test]
        async fn known_signer_fails_closed_past_window() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(holders_body(&[HOLDER], now_secs())),
                )
                .mount(&server)
                .await;
            let client = leak_client(&server).await;

            // Tiny window so the cache ages out within the test. Chain-lag check off.
            let src =
                SubgraphTrustedSigners::new(client, Duration::from_millis(400), Duration::ZERO);
            src.refresh().await.unwrap();
            assert!(
                src.verify_trusted(HOLDER).await.is_ok(),
                "a known holder passes from cache while the window is fresh"
            );

            // The subgraph goes down and the cache ages past the window.
            server.reset().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
            tokio::time::sleep(Duration::from_millis(600)).await;

            assert!(
                matches!(
                    src.verify_trusted(HOLDER).await,
                    Err(DipsError::TrustVerificationUnavailable(_))
                ),
                "past the window with the subgraph down, even a known holder fails closed"
            );
        }

        #[test]
        fn query_role_hash_matches_keccak() {
            use thegraph_core::alloy::primitives::keccak256;
            let expected = format!("0x{:x}", keccak256(b"AGREEMENT_MANAGER_ROLE"));
            assert!(
                ROLE_HOLDERS_QUERY.contains(&expected),
                "query role hash drifted from keccak256(\"AGREEMENT_MANAGER_ROLE\")"
            );
        }
    }
}
