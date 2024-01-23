// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use ethers_core::types::U256;
use eventuals::Eventual;
use sqlx::postgres::PgListener;
use sqlx::{types::BigDecimal, PgPool};
use std::collections::HashSet;
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tap_core::tap_manager::SignedReceipt;
use tokio::sync::RwLock;
use tracing::error;

use crate::{escrow_accounts::EscrowAccounts, prelude::Allocation};

#[derive(Clone)]
pub struct TapManager {
    indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    escrow_accounts: Eventual<EscrowAccounts>,
    pgpool: PgPool,
    domain_separator: Arc<Eip712Domain>,
    sender_denylist: Arc<RwLock<HashSet<Address>>>,
    _sender_denylist_watcher_handle: Arc<tokio::task::JoinHandle<()>>,
    sender_denylist_watcher_cancel_token: tokio_util::sync::CancellationToken,
}

impl TapManager {
    pub async fn new(
        pgpool: PgPool,
        indexer_allocations: Eventual<HashMap<Address, Allocation>>,
        escrow_accounts: Eventual<EscrowAccounts>,
        domain_separator: Eip712Domain,
    ) -> Self {
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
            indexer_allocations,
            escrow_accounts,
            pgpool,
            domain_separator: Arc::new(domain_separator),
            sender_denylist,
            _sender_denylist_watcher_handle: sender_denylist_watcher_handle,
            sender_denylist_watcher_cancel_token,
        }
    }

    /// Checks that the receipt refers to eligible allocation ID and TAP sender.
    ///
    /// If the receipt is valid, it is stored in the database.
    ///
    /// The rest of the TAP receipt checks are expected to be performed out-of-band by the receipt aggregate requester
    /// service.
    pub async fn verify_and_store_receipt(
        &self,
        receipt: SignedReceipt,
    ) -> Result<(), anyhow::Error> {
        let allocation_id = &receipt.message.allocation_id;
        if !self
            .indexer_allocations
            .value()
            .await
            .map(|allocations| allocations.contains_key(allocation_id))
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "Receipt allocation ID `{}` is not eligible for this indexer",
                allocation_id
            ));
        }

        let receipt_signer = receipt
            .recover_signer(self.domain_separator.as_ref())
            .map_err(|e| {
                error!("Failed to recover receipt signer: {}", e);
                anyhow!(e)
            })?;

        let escrow_accounts_snapshot = self.escrow_accounts.value_immediate().unwrap_or_default();

        // We bail if the receipt signer does not have a corresponding sender in the escrow
        // accounts.
        let receipt_sender = escrow_accounts_snapshot.get_sender_for_signer(&receipt_signer)?;

        // Check that the sender has a non-zero balance -- more advanced accounting is done in
        // `tap-agent`.
        if !escrow_accounts_snapshot
            .get_balance_for_sender(&receipt_sender)
            .map_or(false, |balance| balance > U256::zero())
        {
            anyhow::bail!(
                "Receipt sender `{}` does not have a sufficient balance",
                receipt_signer,
            );
        }

        // Check that the sender is not denylisted
        if self.sender_denylist.read().await.contains(&receipt_sender) {
            anyhow::bail!(
                "Received a receipt from a denylisted sender: {}",
                receipt_signer
            );
        }

        // TODO: consider doing this in another async task to avoid slowing down the paid query flow.
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_receipts (allocation_id, signer_address, timestamp_ns, value, receipt)
                VALUES ($1, $2, $3, $4, $5)
            "#,
            format!("{:?}", allocation_id)
                .trim_start_matches("0x")
                .to_owned(),
            receipt_signer
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            BigDecimal::from(receipt.message.timestamp_ns),
            BigDecimal::from_str(&receipt.message.value.to_string())?,
            serde_json::to_value(receipt).map_err(|e| anyhow!(e))?
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            error!("Failed to store receipt: {}", e);
            anyhow!(e)
        })?;

        Ok(())
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

        *(denylist_rwlock.write().await) = sender_denylist;

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
                                .await
                                .insert(denylist_notification.sender_address);
                        }
                        "DELETE" => {
                            denylist
                                .write()
                                .await
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

impl Drop for TapManager {
    fn drop(&mut self) {
        // Clean shutdown for the sender_denylist_watcher
        // Though since it's not a critical task, we don't wait for it to finish (join).
        self.sender_denylist_watcher_cancel_token.cancel();
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use crate::prelude::{AllocationStatus, SubgraphDeployment};
    use alloy_primitives::Address;
    use keccak_hash::H256;
    use sqlx::postgres::PgListener;

    use crate::test_vectors::{self, create_signed_receipt, TAP_SENDER};

    use super::*;

    const ALLOCATION_ID: &str = "0xdeadbeefcafebabedeadbeefcafebabedeadbeef";

    async fn new_tap_manager(pgpool: PgPool) -> TapManager {
        let allocation_id = Address::from_str(ALLOCATION_ID).unwrap();

        // Mock allocation
        let allocation = Allocation {
            id: allocation_id,
            subgraph_deployment: SubgraphDeployment {
                id: *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT,
                denied_at: None,
            },
            status: AllocationStatus::Active,
            allocated_tokens: U256::zero(),
            closed_at_epoch: None,
            closed_at_epoch_start_block_hash: None,
            poi: None,
            previous_epoch_start_block_hash: None,
            created_at_block_hash: H256::zero().to_string(),
            created_at_epoch: 0,
            indexer: *test_vectors::INDEXER_ADDRESS,
            query_fee_rebates: None,
            query_fees_collected: None,
        };
        let indexer_allocations = Eventual::from_value(HashMap::from_iter(
            vec![(allocation_id, allocation)].into_iter(),
        ));

        // Mock escrow accounts
        let escrow_accounts = Eventual::from_value(EscrowAccounts::new(
            test_vectors::ESCROW_ACCOUNTS_BALANCES.to_owned(),
            test_vectors::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        ));

        TapManager::new(
            pgpool.clone(),
            indexer_allocations,
            escrow_accounts,
            test_vectors::TAP_EIP712_DOMAIN.to_owned(),
        )
        .await
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_verify_and_store_receipt(pgpool: PgPool) {
        // Listen to pg_notify events
        let mut listener = PgListener::connect_with(&pgpool).await.unwrap();
        listener
            .listen("scalar_tap_receipt_notification")
            .await
            .unwrap();

        let allocation_id = Address::from_str(ALLOCATION_ID).unwrap();
        let signed_receipt =
            create_signed_receipt(allocation_id, u64::MAX, u64::MAX, u128::MAX).await;

        let tap_manager = new_tap_manager(pgpool.clone()).await;

        tap_manager
            .verify_and_store_receipt(signed_receipt.clone())
            .await
            .unwrap();

        // Check that the receipt DB insertion was notified (PG NOTIFY, see migrations for more info)
        let notification = tokio::time::timeout(std::time::Duration::from_secs(1), listener.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notification.channel(), "scalar_tap_receipt_notification");

        // Deserialize the notification payload (json)
        let notification_payload: serde_json::Value =
            serde_json::from_str(notification.payload()).unwrap();
        assert_eq!(
            // The allocation ID is stored as a hex string in the DB, without the 0x prefix nor checksum, so we parse it
            // into an Address and then back to a string to compare it with the expected value.
            Address::from_str(notification_payload["allocation_id"].as_str().unwrap())
                .unwrap()
                .to_string(),
            allocation_id.to_string()
        );
        assert_eq!(notification_payload["timestamp_ns"], u64::MAX);
        assert!(notification_payload["id"].is_u64());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_sender_denylist(pgpool: PgPool) {
        // Add the sender to the denylist
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            TAP_SENDER.1.to_string().trim_start_matches("0x").to_owned()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        let allocation_id = Address::from_str(ALLOCATION_ID).unwrap();
        let signed_receipt =
            create_signed_receipt(allocation_id, u64::MAX, u64::MAX, u128::MAX).await;

        let tap_manager = new_tap_manager(pgpool.clone()).await;

        // Check that the receipt is rejected
        assert!(tap_manager
            .verify_and_store_receipt(signed_receipt.clone())
            .await
            .is_err());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_sender_denylist_updates(pgpool: PgPool) {
        let allocation_id = Address::from_str(ALLOCATION_ID).unwrap();
        let signed_receipt =
            create_signed_receipt(allocation_id, u64::MAX, u64::MAX, u128::MAX).await;

        let tap_manager = new_tap_manager(pgpool.clone()).await;

        // Check that the receipt is valid
        tap_manager
            .verify_and_store_receipt(signed_receipt.clone())
            .await
            .unwrap();

        // Add the sender to the denylist
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
            TAP_SENDER.1.to_string().trim_start_matches("0x").to_owned()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        // Check that the receipt is rejected
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(tap_manager
            .verify_and_store_receipt(signed_receipt.clone())
            .await
            .is_err());

        // Remove the sender from the denylist
        sqlx::query!(
            r#"
                DELETE FROM scalar_tap_denylist
                WHERE sender_address = $1
            "#,
            TAP_SENDER.1.to_string().trim_start_matches("0x").to_owned()
        )
        .execute(&pgpool)
        .await
        .unwrap();

        // Check that the receipt is valid again
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        tap_manager
            .verify_and_store_receipt(signed_receipt.clone())
            .await
            .unwrap();
    }
}
