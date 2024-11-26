// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::middleware::Sender;
use alloy::primitives::Address;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::RwLock;
use std::{str::FromStr, sync::Arc};
use tap_core::receipt::checks::CheckError;
use tap_core::receipt::{
    checks::{Check, CheckResult},
    state::Checking,
    ReceiptWithState,
};
use tracing::error;

pub struct DenyListCheck {
    sender_denylist: Arc<RwLock<HashSet<Address>>>,
    _sender_denylist_watcher_handle: Arc<tokio::task::JoinHandle<()>>,
    sender_denylist_watcher_cancel_token: tokio_util::sync::CancellationToken,
}

impl DenyListCheck {
    pub async fn new(pgpool: PgPool) -> Self {
        // Listen to pg_notify events. We start it before updating the sender_denylist so that we
        // don't miss any updates. PG will buffer the notifications until we start consuming them.
        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("scalar_tap_deny_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_deny_notification'",
            );

        // Fetch the denylist from the DB
        let sender_denylist = Arc::new(RwLock::new(HashSet::new()));
        Self::sender_denylist_reload(pgpool.clone(), sender_denylist.clone())
            .await
            .expect("should be able to fetch the sender_denylist from the DB on startup");

        let sender_denylist_watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        let sender_denylist_watcher_handle = Arc::new(tokio::spawn(Self::sender_denylist_watcher(
            pgpool.clone(),
            pglistener,
            sender_denylist.clone(),
            sender_denylist_watcher_cancel_token.clone(),
        )));
        Self {
            sender_denylist,
            _sender_denylist_watcher_handle: sender_denylist_watcher_handle,
            sender_denylist_watcher_cancel_token,
        }
    }

    async fn sender_denylist_reload(
        pgpool: PgPool,
        denylist_rwlock: Arc<RwLock<HashSet<Address>>>,
    ) -> anyhow::Result<()> {
        // Fetch the denylist from the DB
        let sender_denylist = sqlx::query!(
            r#"
                SELECT sender_address FROM scalar_tap_denylist
            "#
        )
        .fetch_all(&pgpool)
        .await?
        .iter()
        .map(|row| Address::from_str(&row.sender_address))
        .collect::<Result<HashSet<_>, _>>()?;

        *(denylist_rwlock.write().unwrap()) = sender_denylist;

        Ok(())
    }

    async fn sender_denylist_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        denylist: Arc<RwLock<HashSet<Address>>>,
        cancel_token: tokio_util::sync::CancellationToken,
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
                    'scalar_tap_deny_notification'",
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
                            error!(
                                "Received an unexpected denylist table notification: {}. Reloading entire \
                                denylist.",
                                denylist_notification.tg_op
                            );

                            Self::sender_denylist_reload(pgpool.clone(), denylist.clone())
                                .await
                                .expect("should be able to reload the sender denylist")
                        }
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Check for DenyListCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        _: &ReceiptWithState<Checking>,
    ) -> CheckResult {
        let Sender(receipt_sender) = ctx
            .get::<Sender>()
            .ok_or(CheckError::Failed(anyhow::anyhow!("Could not find sender")))?;

        // Check that the sender is not denylisted
        if self
            .sender_denylist
            .read()
            .unwrap()
            .contains(receipt_sender)
        {
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
    use alloy::hex::ToHexExt;
    use tap_core::receipt::{Context, ReceiptWithState};

    use test_assets::{self, create_signed_receipt, SignedReceiptRequest, TAP_SENDER};

    use super::*;

    async fn new_deny_list_check(pgpool: PgPool) -> DenyListCheck {
        // Mock escrow accounts
        DenyListCheck::new(pgpool).await
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sender_denylist(pgpool: PgPool) {
        // Add the sender to the denylist
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            TAP_SENDER.1.encode_hex()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        let signed_receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;

        let deny_list_check = new_deny_list_check(pgpool.clone()).await;

        let checking_receipt = ReceiptWithState::new(signed_receipt);

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));

        // Check that the receipt is rejected
        assert!(deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .is_err());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sender_denylist_updates(pgpool: PgPool) {
        let signed_receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;

        let deny_list_check = new_deny_list_check(pgpool.clone()).await;

        // Check that the receipt is valid
        let checking_receipt = ReceiptWithState::new(signed_receipt);

        let mut ctx = Context::new();
        ctx.insert(Sender(TAP_SENDER.1));
        deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .unwrap();

        // Add the sender to the denylist
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            TAP_SENDER.1.encode_hex()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        // Check that the receipt is rejected
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .is_err());

        // Remove the sender from the denylist
        sqlx::query!(
            r#"
                DELETE FROM scalar_tap_denylist
                WHERE sender_address = $1
            "#,
            TAP_SENDER.1.encode_hex()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        // Check that the receipt is valid again
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        deny_list_check
            .check(&ctx, &checking_receipt)
            .await
            .unwrap();
    }
}
