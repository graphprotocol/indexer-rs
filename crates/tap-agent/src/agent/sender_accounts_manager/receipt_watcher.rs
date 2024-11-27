// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::agent::{
    metrics::RECEIPTS_CREATED, sender_account::SenderAccountMessage,
    sender_allocation::SenderAllocationMessage,
};
use anyhow::{anyhow, bail, Result};
use indexer_monitor::EscrowAccounts;
use ractor::ActorRef;
use sqlx::postgres::PgListener;
use tokio::sync::watch::Receiver;
use tracing::{error, warn};

use super::NewReceiptNotification;

/// Continuously listens for new receipt notifications from Postgres and forwards them to the
/// corresponding SenderAccount.
pub async fn new_receipts_watcher(
    mut pglistener: PgListener,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<String>,
) {
    loop {
        // TODO: recover from errors or shutdown the whole program?
        let pg_notification = pglistener.recv().await.expect(
            "should be able to receive Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
        );
        let new_receipt_notification: NewReceiptNotification =
            serde_json::from_str(pg_notification.payload()).expect(
                "should be able to deserialize the Postgres Notify event payload as a \
                        NewReceiptNotification",
            );
        if let Err(e) = handle_notification(
            new_receipt_notification,
            escrow_accounts_rx.clone(),
            prefix.as_deref(),
        )
        .await
        {
            error!("{}", e);
        }
    }
}

pub(super) async fn handle_notification(
    new_receipt_notification: NewReceiptNotification,
    escrow_accounts_rx: Receiver<EscrowAccounts>,
    prefix: Option<&str>,
) -> Result<()> {
    tracing::trace!(
        notification = ?new_receipt_notification,
        "New receipt notification detected!"
    );

    let Ok(sender_address) = escrow_accounts_rx
        .borrow()
        .get_sender_for_signer(&new_receipt_notification.signer_address)
    else {
        // TODO: save the receipt in the failed receipts table?
        bail!(
            "No sender address found for receipt signer address {}. \
                    This should not happen.",
            new_receipt_notification.signer_address
        );
    };

    let allocation_id = &new_receipt_notification.allocation_id;
    let allocation_str = &allocation_id.to_string();

    let actor_name = format!(
        "{}{sender_address}:{allocation_id}",
        prefix
            .as_ref()
            .map_or(String::default(), |prefix| format!("{prefix}:"))
    );

    let Some(sender_allocation) = ActorRef::<SenderAllocationMessage>::where_is(actor_name) else {
        warn!(
            "No sender_allocation found for sender_address {}, allocation_id {} to process new \
                receipt notification. Starting a new sender_allocation.",
            sender_address, allocation_id
        );
        let sender_account_name = format!(
            "{}{sender_address}",
            prefix
                .as_ref()
                .map_or(String::default(), |prefix| format!("{prefix}:"))
        );

        let Some(sender_account) = ActorRef::<SenderAccountMessage>::where_is(sender_account_name)
        else {
            bail!(
                "No sender_account was found for address: {}.",
                sender_address
            );
        };
        sender_account
            .cast(SenderAccountMessage::NewAllocationId(*allocation_id))
            .map_err(|e| {
                anyhow!(
                    "Error while sendeing new allocation id message to sender_account: {:?}",
                    e
                )
            })?;
        return Ok(());
    };

    sender_allocation
        .cast(SenderAllocationMessage::NewReceipt(
            new_receipt_notification,
        ))
        .map_err(|e| {
            anyhow::anyhow!(
                "Error while forwarding new receipt notification to sender_allocation: {:?}",
                e
            )
        })?;

    RECEIPTS_CREATED
        .with_label_values(&[&sender_address.to_string(), allocation_str])
        .inc();
    Ok(())
}
