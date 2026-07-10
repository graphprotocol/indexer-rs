// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashSet,
    str::FromStr,
    sync::{Arc, RwLock},
};

use sqlx::{postgres::PgListener, PgPool, Row};
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{alloy::primitives::Address, CollectionId};

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

/// Rejects receipts for a (payer, collection) whose final RAV was already redeemed on-chain.
/// tap-agent marks the closing RAV as `last`, indexer-agent sets `final` once its redemption is
/// on-chain past the finality window; later receipts can never be aggregated and paid.
pub struct AllocationRedeemedCheck {
    finalized_ravs: Arc<RwLock<HashSet<(Address, CollectionId)>>>,
    watcher_cancel_token: tokio_util::sync::CancellationToken,

    #[cfg(test)]
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl AllocationRedeemedCheck {
    pub async fn new(pgpool: PgPool) -> Self {
        // Listen before the initial load so no notification is missed; Postgres buffers
        // notifications until we start consuming them.
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("tap_horizon_rav_final_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'tap_horizon_rav_final_notification'",
            );

        let finalized_ravs = Arc::new(RwLock::new(HashSet::new()));
        Self::finalized_ravs_reload(pgpool.clone(), finalized_ravs.clone())
            .await
            .expect("should be able to fetch finalized RAVs from the DB on startup");

        #[cfg(test)]
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());

        let watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(Self::finalized_ravs_watcher(
            pgpool.clone(),
            pglistener,
            finalized_ravs.clone(),
            watcher_cancel_token.clone(),
            #[cfg(test)]
            notify.clone(),
        ));

        Self {
            finalized_ravs,
            watcher_cancel_token,
            #[cfg(test)]
            notify,
        }
    }

    async fn finalized_ravs_reload(
        pgpool: PgPool,
        finalized_rwlock: Arc<RwLock<HashSet<(Address, CollectionId)>>>,
    ) -> anyhow::Result<()> {
        let rows = sqlx::query(
            r#"
                SELECT payer, collection_id FROM tap_horizon_ravs WHERE final = TRUE
            "#,
        )
        .fetch_all(&pgpool)
        .await?;
        let finalized = rows
            .iter()
            .map(|row| {
                let payer = Address::from_str(row.try_get::<String, _>("payer")?.trim())?;
                let collection =
                    CollectionId::from_str(row.try_get::<String, _>("collection_id")?.trim())?;
                Ok((payer, collection))
            })
            .collect::<anyhow::Result<HashSet<_>>>()?;

        *(finalized_rwlock.write().unwrap()) = finalized;

        Ok(())
    }

    async fn finalized_ravs_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        finalized_ravs: Arc<RwLock<HashSet<(Address, CollectionId)>>>,
        cancel_token: tokio_util::sync::CancellationToken,
        #[cfg(test)] notify: std::sync::Arc<tokio::sync::Notify>,
    ) {
        #[derive(serde::Deserialize)]
        struct RavFinalNotification {
            payer: String,
            collection_id: String,
        }

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }

                pg_notification = pglistener.recv() => {
                    let pg_notification = pg_notification.expect(
                        "should be able to receive Postgres Notify events on the channel \
                        'tap_horizon_rav_final_notification'",
                    );

                    let notification: RavFinalNotification =
                        serde_json::from_str(pg_notification.payload()).expect(
                            "should be able to deserialize the Postgres Notify event payload as a \
                            RavFinalNotification",
                        );

                    match (
                        Address::from_str(notification.payer.trim()),
                        CollectionId::from_str(notification.collection_id.trim()),
                    ) {
                        (Ok(payer), Ok(collection)) => {
                            finalized_ravs.write().unwrap().insert((payer, collection));
                        }
                        // A RAV is never un-finalized, so the set only grows; anything
                        // unparsable means the table changed shape: reload everything.
                        _ => {
                            tracing::error!(
                                payload = %pg_notification.payload(),
                                "Unparsable RAV finalization notification; reloading finalized RAVs"
                            );
                            Self::finalized_ravs_reload(pgpool.clone(), finalized_ravs.clone())
                                .await
                                .expect("should be able to reload finalized RAVs");
                        }
                    }
                    #[cfg(test)]
                    notify.notify_one();
                }
            }
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

        let collection =
            CollectionId::from(receipt.signed_receipt().as_ref().message.collection_id);

        let redeemed = self
            .finalized_ravs
            .read()
            .unwrap()
            .contains(&(*sender, collection));

        if redeemed {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Final RAV already redeemed for collection {} of payer {sender}",
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
    use test_assets::{create_signed_receipt_v2, COLLECTION_ID_0, TAP_SENDER};
    use thegraph_core::alloy::hex::ToHexExt;

    use super::*;

    async fn insert_rav(pgpool: &PgPool, is_final: bool) {
        sqlx::query(
            r#"
                INSERT INTO tap_horizon_ravs (
                    signature, collection_id, payer, data_service, service_provider,
                    timestamp_ns, value_aggregate, metadata, last, final
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9)
                ON CONFLICT (payer, data_service, service_provider, collection_id)
                DO UPDATE SET final = $9
            "#,
        )
        .bind(vec![0u8; 65])
        .bind(COLLECTION_ID_0.encode_hex())
        .bind(TAP_SENDER.1.encode_hex())
        .bind(test_assets::VERIFIER_ADDRESS.encode_hex())
        .bind(test_assets::INDEXER_ADDRESS.encode_hex())
        .bind(1_000_000_000i64)
        .bind(100i64)
        .bind(vec![0u8; 1])
        .bind(is_final)
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
    async fn finalized_rav_rejects_receipt() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_rav(&test_db.pool, true).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone()).await;

        let result = check.check(&sender_ctx(), &checking_receipt().await).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already redeemed"));
    }

    #[tokio::test]
    async fn non_final_rav_passes_receipt() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_rav(&test_db.pool, false).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone()).await;

        assert!(check
            .check(&sender_ctx(), &checking_receipt().await)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn finalization_is_picked_up_live() {
        let test_db = test_assets::setup_shared_test_db().await;
        let check = AllocationRedeemedCheck::new(test_db.pool.clone()).await;

        let receipt = checking_receipt().await;
        assert!(check.check(&sender_ctx(), &receipt).await.is_ok());

        insert_rav(&test_db.pool, true).await;
        check.notify.notified().await;

        assert!(check.check(&sender_ctx(), &receipt).await.is_err());
    }

    #[tokio::test]
    async fn missing_sender_in_context_rejects() {
        let test_db = test_assets::setup_shared_test_db().await;
        insert_rav(&test_db.pool, true).await;

        let check = AllocationRedeemedCheck::new(test_db.pool.clone()).await;

        let result = check
            .check(&Context::new(), &checking_receipt().await)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing sender"));
    }
}
