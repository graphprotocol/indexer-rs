// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashSet,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration,
};

use sqlx::{postgres::PgListener, PgPool, Row};
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{alloy::primitives::Address, CollectionId};

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

/// A (payer, data_service, collection) whose closing RAV state is tracked.
type RavKey = (Address, Address, CollectionId);

/// Rejects receipts whose (payer, data_service, collection) closing RAV was already redeemed
/// on-chain; such receipts can never be paid. Eligibility routinely rejects first (redemption
/// follows the same buffer), so this is insurance against out-of-band redemptions and skew.
pub struct AllocationRedeemedCheck {
    redeemed_ravs: Arc<RwLock<HashSet<RavKey>>>,
    watcher_cancel_token: tokio_util::sync::CancellationToken,

    #[cfg(test)]
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl AllocationRedeemedCheck {
    pub async fn new(pgpool: PgPool, service_provider: Address) -> Self {
        // Listen before the initial load so no notification is missed; Postgres buffers
        // notifications until we start consuming them.
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("tap_horizon_rav_redeemed_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'tap_horizon_rav_redeemed_notification'",
            );

        let redeemed_ravs = Arc::new(RwLock::new(HashSet::new()));
        Self::redeemed_ravs_reload(&pgpool, service_provider, &redeemed_ravs)
            .await
            .expect("should be able to fetch redeemed closing RAVs from the DB on startup");

        #[cfg(test)]
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());

        let watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(Self::redeemed_ravs_watcher(
            pgpool.clone(),
            service_provider,
            pglistener,
            redeemed_ravs.clone(),
            watcher_cancel_token.clone(),
            #[cfg(test)]
            notify.clone(),
        ));

        Self {
            redeemed_ravs,
            watcher_cancel_token,
            #[cfg(test)]
            notify,
        }
    }

    async fn redeemed_ravs_reload(
        pgpool: &PgPool,
        service_provider: Address,
        redeemed_rwlock: &Arc<RwLock<HashSet<RavKey>>>,
    ) -> anyhow::Result<()> {
        use thegraph_core::alloy::hex::ToHexExt;
        let rows = sqlx::query(
            r#"
                SELECT payer, data_service, collection_id FROM tap_horizon_ravs
                WHERE last = TRUE AND redeemed_at IS NOT NULL AND service_provider = $1
            "#,
        )
        .bind(service_provider.encode_hex())
        .fetch_all(pgpool)
        .await?;
        let redeemed = rows
            .iter()
            .map(|row| {
                let payer = Address::from_str(row.try_get::<String, _>("payer")?.trim())?;
                let data_service =
                    Address::from_str(row.try_get::<String, _>("data_service")?.trim())?;
                let collection =
                    CollectionId::from_str(row.try_get::<String, _>("collection_id")?.trim())?;
                Ok((payer, data_service, collection))
            })
            .collect::<anyhow::Result<HashSet<_>>>()?;

        *(redeemed_rwlock.write().unwrap()) = redeemed;

        Ok(())
    }

    async fn redeemed_ravs_watcher(
        pgpool: PgPool,
        service_provider: Address,
        mut pglistener: PgListener,
        redeemed_ravs: Arc<RwLock<HashSet<RavKey>>>,
        cancel_token: tokio_util::sync::CancellationToken,
        #[cfg(test)] notify: std::sync::Arc<tokio::sync::Notify>,
    ) {
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }

                pg_notification = pglistener.try_recv() => {
                    match pg_notification {
                        Ok(Some(notification)) => {
                            Self::apply_notification(
                                notification.payload(),
                                service_provider,
                                &redeemed_ravs,
                            );
                        }
                        // The connection dropped and notifications may have been lost while
                        // it was down; resync the whole set once it is back.
                        Ok(None) => {
                            tracing::warn!(
                                "Lost the Postgres notification connection; reloading redeemed RAVs"
                            );
                            while let Err(error) =
                                Self::redeemed_ravs_reload(&pgpool, service_provider, &redeemed_ravs)
                                    .await
                            {
                                tracing::error!(
                                    %error,
                                    "Failed to reload redeemed RAVs after reconnect; retrying"
                                );
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                %error,
                                "Error receiving Postgres notifications for redeemed RAVs; retrying"
                            );
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                    #[cfg(test)]
                    notify.notify_one();
                }
            }
        }
    }

    fn apply_notification(
        payload: &str,
        service_provider: Address,
        redeemed_ravs: &Arc<RwLock<HashSet<RavKey>>>,
    ) {
        #[derive(serde::Deserialize)]
        struct RavRedeemedNotification {
            // False when a redemption reorgs out and indexer-agent clears redeemed_at.
            redeemed: bool,
            payer: String,
            data_service: String,
            service_provider: String,
            collection_id: String,
        }

        let Ok(notification) = serde_json::from_str::<RavRedeemedNotification>(payload) else {
            tracing::error!(payload, "Unparsable RAV redemption notification; ignoring");
            return;
        };
        let parsed = (
            Address::from_str(notification.service_provider.trim()),
            Address::from_str(notification.payer.trim()),
            Address::from_str(notification.data_service.trim()),
            CollectionId::from_str(notification.collection_id.trim()),
        );
        let (Ok(provider), Ok(payer), Ok(data_service), Ok(collection)) = parsed else {
            tracing::error!(payload, "Unparsable RAV redemption notification; ignoring");
            return;
        };

        // RAVs for another service provider are not receipts this service can see.
        if provider != service_provider {
            return;
        }

        let key = (payer, data_service, collection);
        if notification.redeemed {
            redeemed_ravs.write().unwrap().insert(key);
        } else {
            redeemed_ravs.write().unwrap().remove(&key);
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationRedeemedCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(sender) = ctx
            .get::<Sender>()
            .ok_or_else(|| CheckError::Failed(anyhow::anyhow!("Missing sender in context")))?;

        let message = &receipt.signed_receipt().as_ref().message;
        let collection = CollectionId::from(message.collection_id);
        let key = (*sender, message.data_service, collection);

        if self.redeemed_ravs.read().unwrap().contains(&key) {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Closing RAV already redeemed for collection {} of payer {sender}",
                collection.as_address()
            )));
        }
        Ok(())
    }
}

impl Drop for AllocationRedeemedCheck {
    fn drop(&mut self) {
        self.watcher_cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{
        create_signed_receipt_v2, COLLECTION_ID_0, INDEXER_ADDRESS, TAP_SENDER, VERIFIER_ADDRESS,
    };
    use thegraph_core::alloy::hex::ToHexExt;

    use super::*;

    async fn insert_last_rav(pgpool: &PgPool, service_provider: Address, redeemed: bool) {
        sqlx::query(
            r#"
                INSERT INTO tap_horizon_ravs (
                    signature, collection_id, payer, data_service, service_provider,
                    timestamp_ns, value_aggregate, metadata, last, redeemed_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, CASE WHEN $9 THEN NOW() ELSE NULL END)
            "#,
        )
        .bind(vec![0u8; 65])
        .bind(COLLECTION_ID_0.encode_hex())
        .bind(TAP_SENDER.1.encode_hex())
        .bind(VERIFIER_ADDRESS.encode_hex())
        .bind(service_provider.encode_hex())
        .bind(1_000_000_000i64)
        .bind(100i64)
        .bind(vec![0u8; 1])
        .bind(redeemed)
        .execute(pgpool)
        .await
        .unwrap();
    }

    // The statement shape indexer-agent runs when a redemption lands or reorgs out.
    async fn agent_sets_redeemed_at(pgpool: &PgPool, redeemed: bool) {
        let set = if redeemed { "NOW()" } else { "NULL" };
        sqlx::query(&format!(
            "UPDATE tap_horizon_ravs SET redeemed_at = {set} WHERE payer = $1 AND collection_id = $2"
        ))
        .bind(TAP_SENDER.1.encode_hex())
        .bind(COLLECTION_ID_0.encode_hex())
        .execute(pgpool)
        .await
        .unwrap();
    }

    async fn checking_receipt() -> CheckingReceipt {
        let receipt = create_signed_receipt_v2()
            .collection_id(COLLECTION_ID_0)
            .call()
            .await;
        CheckingReceipt::new(TapReceipt(receipt))
    }

    fn sender_ctx() -> Context {
        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));
        ctx
    }

    #[tokio::test]
    async fn redeemed_closing_rav_rejects_receipt() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_last_rav(&test_db.pool, INDEXER_ADDRESS, true).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone(), INDEXER_ADDRESS).await;

        let result = check.check(&sender_ctx(), &checking_receipt().await).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already redeemed"));
    }

    #[tokio::test]
    async fn unredeemed_rav_passes_receipt() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_last_rav(&test_db.pool, INDEXER_ADDRESS, false).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone(), INDEXER_ADDRESS).await;

        assert!(check
            .check(&sender_ctx(), &checking_receipt().await)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn other_service_provider_rav_passes_receipt() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_last_rav(&test_db.pool, Address::from([0x33u8; 20]), true).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone(), INDEXER_ADDRESS).await;

        assert!(check
            .check(&sender_ctx(), &checking_receipt().await)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn redemption_and_reorg_are_picked_up_live() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_last_rav(&test_db.pool, INDEXER_ADDRESS, false).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone(), INDEXER_ADDRESS).await;

        let receipt = checking_receipt().await;
        assert!(check.check(&sender_ctx(), &receipt).await.is_ok());

        // Redemption lands on-chain: receipts start being rejected.
        agent_sets_redeemed_at(&test_db.pool, true).await;
        check.notify.notified().await;
        assert!(check.check(&sender_ctx(), &receipt).await.is_err());

        // The redemption reorgs out and indexer-agent clears redeemed_at:
        // receipts are accepted again.
        agent_sets_redeemed_at(&test_db.pool, false).await;
        check.notify.notified().await;
        assert!(check.check(&sender_ctx(), &receipt).await.is_ok());
    }

    #[tokio::test]
    async fn missing_sender_in_context_rejects() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_last_rav(&test_db.pool, INDEXER_ADDRESS, true).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone(), INDEXER_ADDRESS).await;

        let result = check
            .check(&Context::new(), &checking_receipt().await)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing sender"));
    }
}
