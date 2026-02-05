// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashSet,
    str::FromStr,
    sync::{Arc, RwLock},
};

use sqlx::{postgres::PgListener, PgPool, Row};
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::alloy::primitives::Address;

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

pub struct DenyListCheck {
    sender_denylist_v2: Arc<RwLock<HashSet<Address>>>,
    sender_denylist_watcher_cancel_token: tokio_util::sync::CancellationToken,

    #[cfg(test)]
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl DenyListCheck {
    pub async fn new(pgpool: PgPool) -> Self {
        // Listen to pg_notify events. We start it before updating the sender_denylist so that we
        // don't miss any updates. PG will buffer the notifications until we start consuming them.
        let mut pglistener_v2 = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener_v2
            .listen("tap_horizon_deny_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'tap_horizon_deny_notification'",
            );

        // Fetch the denylist from the DB
        let sender_denylist_v2 = Arc::new(RwLock::new(HashSet::new()));
        Self::sender_denylist_reload_v2(pgpool.clone(), sender_denylist_v2.clone())
            .await
            .expect("should be able to fetch the sender denylist from the DB on startup");

        #[cfg(test)]
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());

        let sender_denylist_watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(Self::sender_denylist_watcher(
            pgpool.clone(),
            pglistener_v2,
            sender_denylist_v2.clone(),
            sender_denylist_watcher_cancel_token.clone(),
            #[cfg(test)]
            notify.clone(),
        ));

        Self {
            sender_denylist_v2,
            sender_denylist_watcher_cancel_token,
            #[cfg(test)]
            notify,
        }
    }

    async fn sender_denylist_reload_v2(
        pgpool: PgPool,
        denylist_rwlock: Arc<RwLock<HashSet<Address>>>,
    ) -> anyhow::Result<()> {
        // Fetch the denylist from the DB
        let rows = sqlx::query(
            r#"
                SELECT sender_address FROM tap_horizon_denylist
            "#,
        )
        .fetch_all(&pgpool)
        .await?;
        let sender_denylist = rows
            .iter()
            .map(|row| row.try_get::<String, _>("sender_address"))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(|address| Address::from_str(address))
            .collect::<Result<HashSet<_>, _>>()?;

        *(denylist_rwlock.write().unwrap()) = sender_denylist;

        Ok(())
    }

    async fn sender_denylist_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        denylist: Arc<RwLock<HashSet<Address>>>,
        cancel_token: tokio_util::sync::CancellationToken,
        #[cfg(test)] notify: std::sync::Arc<tokio::sync::Notify>,
    ) {
        #[derive(serde::Deserialize)]
        struct DenylistNotification {
            tg_op: String,
            sender_address: Address,
        }

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }

                pg_notification = pglistener.recv() => {
                    let pg_notification = pg_notification.expect(
                    "should be able to receive Postgres Notify events on the channel \
                    'tap_horizon_deny_notification'",
                    );

                    let denylist_notification: DenylistNotification =
                        serde_json::from_str(pg_notification.payload()).expect(
                            "should be able to deserialize the Postgres Notify event payload as a \
                            DenylistNotification",
                        );

                    match denylist_notification.tg_op.as_str() {
                        "INSERT" => {
                            denylist
                                .write()
                                .unwrap()
                                .insert(denylist_notification.sender_address);
                        }
                        "DELETE" => {
                            denylist
                                .write()
                                .unwrap()
                                .remove(&denylist_notification.sender_address);
                        }
                        // UPDATE and TRUNCATE are not expected to happen. Reload the entire denylist.
                        _ => {
                            tracing::error!(
                                operation = %denylist_notification.tg_op,
                                "Unexpected denylist table notification; reloading denylist"
                            );
                            Self::sender_denylist_reload_v2(pgpool.clone(), denylist.clone())
                                .await
                                .expect("should be able to reload the sender denylist");
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
impl Check<TapReceipt> for DenyListCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        _receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(receipt_sender) = ctx
            .get::<Sender>()
            .ok_or(CheckError::Failed(anyhow::anyhow!("Could not find sender")))?;

        let denied = self
            .sender_denylist_v2
            .read()
            .unwrap()
            .contains(receipt_sender);

        // Check that the sender is not denylisted
        if denied {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Received a receipt from a denylisted sender: {}",
                receipt_sender
            )));
        }

        Ok(())
    }
}

impl Drop for DenyListCheck {
    fn drop(&mut self) {
        // Clean shutdown for the sender_denylist_watcher
        // Though since it's not a critical task, we don't wait for it to finish (join).
        self.sender_denylist_watcher_cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{self, create_signed_receipt_v2, TAP_SENDER};
    use thegraph_core::alloy::hex::ToHexExt;

    use super::*;

    async fn new_deny_list_check(pgpool: PgPool) -> DenyListCheck {
        // Mock escrow accounts
        DenyListCheck::new(pgpool).await
    }

    #[tokio::test]
    async fn test_sender_denylist() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        // Add the sender to the denylist
        sqlx::query(
            r#"
                INSERT INTO tap_horizon_denylist (sender_address)
                VALUES ($1)
            "#,
        )
        .bind(TAP_SENDER.1.encode_hex())
        .execute(&pgpool)
        .await
        .unwrap();

        let signed_receipt = create_signed_receipt_v2().call().await;

        let deny_list_check = new_deny_list_check(pgpool.clone()).await;

        let checking_receipt = CheckingReceipt::new(TapReceipt::V2(signed_receipt));

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        // Check that the receipt is rejected
        assert!(deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_sender_denylist_updates() {
        let test_db = test_assets::setup_shared_test_db().await;
        let pgpool = test_db.pool;
        let signed_receipt = create_signed_receipt_v2().call().await;

        let deny_list_check = new_deny_list_check(pgpool.clone()).await;

        // Check that the receipt is valid
        let checking_receipt = CheckingReceipt::new(TapReceipt::V2(signed_receipt));

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));
        deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .unwrap();

        // Add the sender to the denylist
        sqlx::query(
            r#"
                INSERT INTO tap_horizon_denylist (sender_address)
                VALUES ($1)
            "#,
        )
        .bind(TAP_SENDER.1.encode_hex())
        .execute(&pgpool)
        .await
        .unwrap();

        deny_list_check.notify.notified().await;

        // Check that the receipt is rejected
        assert!(deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .is_err());

        // Remove the sender from the denylist
        sqlx::query(
            r#"
                DELETE FROM tap_horizon_denylist
                WHERE sender_address = $1
            "#,
        )
        .bind(TAP_SENDER.1.encode_hex())
        .execute(&pgpool)
        .await
        .unwrap();

        deny_list_check.notify.notified().await;

        // Check that the receipt is valid again
        assert!(deny_list_check.check(&ctx, &checking_receipt).await.is_ok());
    }
}
