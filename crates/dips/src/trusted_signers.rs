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
        collections::{HashMap, HashSet},
        sync::{Arc, RwLock},
        time::Duration,
    };

    use async_trait::async_trait;
    use indexer_monitor::SubgraphClient;
    use indexer_query::agreement_manager_roles::{self, AgreementManagerRolesQuery};
    use thegraph_core::alloy::primitives::{keccak256, Address};
    use tokio::{sync::Mutex, time::Instant};

    use super::TrustedSignerSource;
    use crate::DipsError;

    /// Page size for the holder query. The manager set is tiny in practice, so one
    /// page almost always suffices; the fetch still pages so a larger set can't be
    /// silently truncated.
    const ROLE_PAGE_SIZE: i64 = 1000;

    /// After a failed on-demand fetch, don't re-hit the subgraph again for this
    /// long, so a burst of unknown signers can't turn into a fetch storm.
    const REFRESH_DEBOUNCE: Duration = Duration::from_secs(5);

    /// How long a "not a role holder" answer is reused from memory before that
    /// signer is re-checked. Caps how often a repeated unknown signer drives a
    /// fetch, and bounds how long a later-granted signer waits to be recognised.
    const REJECTED_SIGNER_TTL: Duration = Duration::from_secs(3600);

    /// Hard cap on the negative cache so a flood of distinct unknown signers
    /// can't grow it without bound.
    const MAX_REJECTED_SIGNERS: usize = 100_000;

    #[derive(Default)]
    struct RoleCache {
        holders: HashSet<Address>,
        /// When the holder set was last fetched successfully.
        last_success: Option<Instant>,
        /// When a fetch most recently failed (for debouncing retries).
        last_failed_attempt: Option<Instant>,
        /// Signers recently confirmed *not* to hold the role, with the time of
        /// that confirmation. A repeat of the same signer is rejected from here
        /// until [`REJECTED_SIGNER_TTL`] passes; bounded by [`MAX_REJECTED_SIGNERS`].
        rejected: HashMap<Address, Instant>,
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
        /// Holder-query page size. Production uses [`ROLE_PAGE_SIZE`]; tests set a
        /// small value to exercise pagination.
        page_size: i64,
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
            Self::new_with_page_size(subgraph, max_stale, max_chain_lag, ROLE_PAGE_SIZE)
        }

        fn new_with_page_size(
            subgraph: &'static SubgraphClient,
            max_stale: Duration,
            max_chain_lag: Duration,
            page_size: i64,
        ) -> Arc<Self> {
            Arc::new(Self {
                subgraph,
                cache: Arc::new(RwLock::new(RoleCache::default())),
                refresh_lock: Arc::new(Mutex::new(())),
                max_stale,
                max_chain_lag,
                page_size,
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

        /// Record a definitive "not a role holder" so a repeat of the same signer
        /// is answered from memory until it ages out. Prunes expired entries and
        /// refuses to grow past a hard cap, bounding the map under a flood.
        fn note_rejected(&self, signer: Address) {
            let mut cache = self.cache.write().unwrap();
            if cache.rejected.len() >= MAX_REJECTED_SIGNERS {
                cache
                    .rejected
                    .retain(|_, t| t.elapsed() < REJECTED_SIGNER_TTL);
                if cache.rejected.len() >= MAX_REJECTED_SIGNERS {
                    tracing::warn!(
                        cap = MAX_REJECTED_SIGNERS,
                        "rejected-signer cache is full; not caching this rejection"
                    );
                    return;
                }
            }
            cache.rejected.insert(signer, Instant::now());
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
            // Derive the role id from its name so it can never drift from the
            // on-chain AGREEMENT_MANAGER_ROLE constant.
            let role = format!("0x{:x}", keccak256(b"AGREEMENT_MANAGER_ROLE"));

            let mut holders = HashSet::new();
            let mut last = String::new();
            let mut block_hash: Option<String> = None;
            let mut head_timestamp: Option<i64> = None;
            let mut first_page = true;

            loop {
                let data = self
                    .subgraph
                    .query::<AgreementManagerRolesQuery, _>(agreement_manager_roles::Variables {
                        role: role.clone(),
                        first: self.page_size,
                        last: last.clone(),
                        block: block_hash.clone().map(|hash| {
                            agreement_manager_roles::Block_height {
                                hash: Some(hash),
                                number: None,
                                number_gte: None,
                            }
                        }),
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("role-holder query failed: {e}"))?;

                // The first page fixes the head timestamp (for the staleness guard)
                // and the block that later pages pin to for a consistent read.
                if first_page {
                    first_page = false;
                    if let Some(meta) = data.meta.as_ref() {
                        head_timestamp = meta.block.timestamp;
                        block_hash = meta.block.hash.clone();
                    }
                }

                let page_len = data.role_assignments.len();
                for row in data.role_assignments {
                    last = row.id;
                    let account = row.account.parse::<Address>().map_err(|e| {
                        anyhow::anyhow!("invalid role-holder account {:?}: {e}", row.account)
                    })?;
                    holders.insert(account);
                }
                if (page_len as i64) < self.page_size {
                    break;
                }
            }

            // A badly-lagging subgraph may be missing recent grants or revokes,
            // so treat it as unreliable rather than trusting a stale role set.
            let max_lag = self.max_chain_lag.as_secs() as i64;
            if max_lag > 0 {
                let timestamp = head_timestamp.ok_or_else(|| {
                    anyhow::anyhow!(
                        "indexing-payments subgraph returned no block timestamp; \
                         cannot verify agreement-manager role freshness"
                    )
                })?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("system time before unix epoch: {e}"))?
                    .as_secs() as i64;
                let lag = now - timestamp;
                if lag > max_lag {
                    return Err(anyhow::anyhow!(
                        "indexing-payments subgraph is {lag}s behind wall-clock (max {max_lag}s); \
                         treating agreement-manager role data as unreliable"
                    ));
                }
            }

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
            // Fast path, answered from memory: a known holder inside the window
            // passes; a signer recently confirmed *not* a holder is rejected, so a
            // repeated unknown signer can't drive a fetch until its entry ages out.
            {
                let cache = self.cache.read().unwrap();
                if cache.holders.contains(&signer) && self.is_fresh(cache.last_success) {
                    return Ok(());
                }
                if cache
                    .rejected
                    .get(&signer)
                    .is_some_and(|t| t.elapsed() < REJECTED_SIGNER_TTL)
                {
                    return Err(DipsError::SenderNotTrusted { signer });
                }
            }

            // A new (or newly-expired) signer, or a cache gone stale past the
            // window: try a single-flighted refresh, then decide against the
            // freshest data.
            let refreshed = self.refresh().await;

            let (present, fresh) = {
                let cache = self.cache.read().unwrap();
                (
                    cache.holders.contains(&signer),
                    self.is_fresh(cache.last_success),
                )
            };

            match refreshed {
                // Authoritative answer from a fresh fetch: a newly-granted signer
                // clears any stale negative entry.
                Ok(()) if present => {
                    self.cache.write().unwrap().rejected.remove(&signer);
                    Ok(())
                }
                Ok(()) => {
                    self.note_rejected(signer);
                    Err(DipsError::SenderNotTrusted { signer })
                }
                // Couldn't refresh: fail open only for a known holder still inside
                // the window; otherwise report transient -- and record no negative
                // entry, since an inability to verify is not a definitive "no".
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

    #[cfg(test)]
    mod test {
        use std::time::Duration;

        use indexer_monitor::{DeploymentDetails, SubgraphClient};
        use serde_json::json;
        use thegraph_core::alloy::primitives::{address, Address};
        use wiremock::{
            matchers::{body_partial_json, method},
            Mock, MockServer, ResponseTemplate,
        };

        use super::*;

        const HOLDER: Address = address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        const STRANGER: Address = address!("0000000000000000000000000000000000000099");

        fn now_secs() -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        }

        /// A graphql_client response body for the role query: one `roleAssignments`
        /// row per account (with a synthetic id cursor) plus the `_meta` head.
        fn holders_body(accounts: &[Address], timestamp: i64) -> serde_json::Value {
            roles_page(accounts, 1, timestamp)
        }

        /// Like [`holders_body`] but with id cursors starting at `first_id`, so a
        /// test can stitch several pages together (ids must strictly increase).
        fn roles_page(accounts: &[Address], first_id: usize, timestamp: i64) -> serde_json::Value {
            json!({
                "data": {
                    "meta": { "block": { "number": 1, "hash": null, "timestamp": timestamp } },
                    "roleAssignments": accounts
                        .iter()
                        .enumerate()
                        .map(|(i, a)| json!({
                            "id": format!("0x{:064x}", first_id + i),
                            "account": a,
                        }))
                        .collect::<Vec<_>>(),
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

        #[tokio::test]
        async fn paginates_through_all_holders() {
            // Page size 2 against three holders forces a second page: page 1 is
            // full (ids 1-2), page 2 partial (id 3), selected by the id_gt cursor
            // carried in the request body. All three must end up trusted.
            const A: Address = address!("0000000000000000000000000000000000000001");
            const B: Address = address!("0000000000000000000000000000000000000002");
            const C: Address = address!("0000000000000000000000000000000000000003");

            let server = MockServer::start().await;
            // First page: empty cursor.
            Mock::given(method("POST"))
                .and(body_partial_json(json!({ "variables": { "last": "" } })))
                .respond_with(ResponseTemplate::new(200).set_body_json(roles_page(
                    &[A, B],
                    1,
                    now_secs(),
                )))
                .mount(&server)
                .await;
            // Second page: cursor is page 1's last id.
            let page1_last = format!("0x{:064x}", 2);
            Mock::given(method("POST"))
                .and(body_partial_json(
                    json!({ "variables": { "last": page1_last } }),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(roles_page(
                    &[C],
                    3,
                    now_secs(),
                )))
                .mount(&server)
                .await;
            let client = leak_client(&server).await;

            let src = SubgraphTrustedSigners::new_with_page_size(
                client,
                Duration::from_secs(60),
                Duration::ZERO,
                2,
            );
            src.refresh().await.unwrap();

            for holder in [A, B, C] {
                assert!(
                    src.verify_trusted(holder).await.is_ok(),
                    "holder {holder} from a later page should be trusted"
                );
            }
        }

        #[tokio::test]
        async fn repeat_unknown_signer_is_not_refetched() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(holders_body(&[HOLDER], now_secs())),
                )
                .mount(&server)
                .await;
            let client = leak_client(&server).await;
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);

            // First lookup: cold cache, one on-demand fetch, definitive reject.
            assert!(matches!(
                src.verify_trusted(STRANGER).await,
                Err(DipsError::SenderNotTrusted { .. })
            ));
            // Second lookup of the same signer: served from the negative cache.
            assert!(matches!(
                src.verify_trusted(STRANGER).await,
                Err(DipsError::SenderNotTrusted { .. })
            ));

            let posts = server.received_requests().await.unwrap().len();
            assert_eq!(posts, 1, "a repeat of a known-bad signer must not re-query");
        }

        #[tokio::test]
        async fn concurrent_unknown_signers_coalesce_one_fetch() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(holders_body(&[HOLDER], now_secs()))
                        .set_delay(Duration::from_millis(200)),
                )
                .mount(&server)
                .await;
            let client = leak_client(&server).await;
            let src = SubgraphTrustedSigners::new(client, Duration::from_secs(60), Duration::ZERO);

            // Two distinct unknown signers arriving together coalesce onto one
            // fetch via the single-flight refresh lock.
            let a = address!("00000000000000000000000000000000000000aa");
            let b = address!("00000000000000000000000000000000000000bb");
            let (ra, rb) = tokio::join!(src.verify_trusted(a), src.verify_trusted(b));
            assert!(matches!(ra, Err(DipsError::SenderNotTrusted { .. })));
            assert!(matches!(rb, Err(DipsError::SenderNotTrusted { .. })));

            let posts = server.received_requests().await.unwrap().len();
            assert_eq!(
                posts, 1,
                "concurrent unknown signers should share a single fetch"
            );
        }
    }
}
