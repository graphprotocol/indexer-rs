// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::sync::Mutex as StdMutex;
use std::{collections::HashMap, str::FromStr, sync::Arc};

use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::prelude::{Allocation, SubgraphClient};
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use thegraph::types::Address;
use tracing::{error, warn};

use crate::config;
use crate::tap::escrow_adapter::EscrowAdapter;
use crate::tap::sender_account::SenderAccount;

#[derive(Deserialize, Debug)]
pub struct NewReceiptNotification {
    pub id: u64,
    pub allocation_id: Address,
    pub signer_address: Address,
    pub timestamp_ns: u64,
    pub value: u128,
}

pub struct SenderAccountsManager {
    _inner: Arc<Inner>,
    new_receipts_watcher_handle: tokio::task::JoinHandle<()>,
    _eligible_allocations_senders_pipe: PipeHandle,
}

/// Inner struct for SenderAccountsManager. This is used to store an Arc state for spawning async
/// tasks.
struct Inner {
    config: &'static config::Cli,
    pgpool: PgPool,
    /// Map of sender_address to SenderAllocation.
    sender_accounts: Arc<StdMutex<HashMap<Address, Arc<SenderAccount>>>>,
    indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    tap_eip712_domain_separator: Eip712Domain,
    sender_aggregator_endpoints: HashMap<Address, String>,
}

impl Inner {
    async fn update_sender_accounts(
        &self,
        indexer_allocations: HashMap<Address, Allocation>,
        target_senders: HashSet<Address>,
    ) -> Result<()> {
        let eligible_allocations: HashSet<Address> = indexer_allocations.keys().copied().collect();
        let mut sender_accounts_copy = self.sender_accounts.lock().unwrap().clone();

        // For all Senders that are not in the target_senders HashSet, set all their allocations to
        // ineligible. That will trigger a finalization of all their receipts.
        for (sender_id, sender_account) in sender_accounts_copy.iter() {
            if !target_senders.contains(sender_id) {
                sender_account.update_allocations(HashSet::new()).await;
            }
        }

        // Get or create SenderAccount instances for all currently eligible
        // senders.
        for sender_id in &target_senders {
            let sender =
                sender_accounts_copy
                    .entry(*sender_id)
                    .or_insert(Arc::new(SenderAccount::new(
                        self.config,
                        self.pgpool.clone(),
                        *sender_id,
                        self.escrow_accounts.clone(),
                        self.escrow_subgraph,
                        self.escrow_adapter.clone(),
                        self.tap_eip712_domain_separator.clone(),
                        self.sender_aggregator_endpoints
                            .get(sender_id)
                            .ok_or_else(|| {
                                anyhow!(
                                    "No sender_aggregator_endpoint found for sender {}",
                                    sender_id
                                )
                            })?
                            .clone(),
                    )));

            // Update sender's allocations
            sender
                .update_allocations(eligible_allocations.clone())
                .await;
        }

        // Replace the sender_accounts with the updated sender_accounts_copy
        *self.sender_accounts.lock().unwrap() = sender_accounts_copy;

        // TODO: remove Sender instances that are finished. Ideally done in another async task?

        Ok(())
    }
}

impl SenderAccountsManager {
    pub async fn new(
        config: &'static config::Cli,
        pgpool: PgPool,
        indexer_allocations: Eventual<HashMap<Address, Allocation>>,
        escrow_accounts: Eventual<EscrowAccounts>,
        escrow_subgraph: &'static SubgraphClient,
        tap_eip712_domain_separator: Eip712Domain,
        sender_aggregator_endpoints: HashMap<Address, String>,
    ) -> Self {
        let escrow_adapter = EscrowAdapter::new(escrow_accounts.clone());

        let inner = Arc::new(Inner {
            config,
            pgpool,
            sender_accounts: Arc::new(StdMutex::new(HashMap::new())),
            indexer_allocations,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            tap_eip712_domain_separator,
            sender_aggregator_endpoints,
        });

        // Listen to pg_notify events. We start it before updating the unaggregated_fees for all
        // SenderAccount instances, so that we don't miss any receipts. PG will buffer the\
        // notifications until we start consuming them with `new_receipts_watcher`.
        let mut pglistener = PgListener::connect_with(&inner.pgpool.clone())
            .await
            .unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );

        let escrow_accounts_snapshot = inner
            .escrow_accounts
            .value()
            .await
            .expect("Should get escrow accounts from Eventual");

        // Gather all outstanding receipts and unfinalized RAVs from the database.
        // Used to create SenderAccount instances for all senders that have unfinalized allocations
        // and try to finalize them if they have become ineligible.

        // First we accumulate all allocations for each sender. This is because we may have more
        // than one signer per sender in DB.
        let mut unfinalized_sender_allocations_map: HashMap<Address, HashSet<Address>> =
            HashMap::new();

        let receipts_signer_allocations_in_db = sqlx::query!(
            r#"
                SELECT DISTINCT 
                    signer_address,
                    (
                        SELECT ARRAY 
                        (
                            SELECT DISTINCT allocation_id
                            FROM scalar_tap_receipts
                            WHERE signer_address = top.signer_address
                        )
                    ) AS allocation_ids
                FROM scalar_tap_receipts AS top
            "#
        )
        .fetch_all(&inner.pgpool)
        .await
        .expect("should be able to fetch pending receipts from the database");

        for row in receipts_signer_allocations_in_db {
            let allocation_ids = row
                .allocation_ids
                .expect("all receipts should have an allocation_id")
                .iter()
                .map(|allocation_id| {
                    Address::from_str(allocation_id)
                        .expect("allocation_id should be a valid address")
                })
                .collect::<HashSet<Address>>();
            let signer_id = Address::from_str(&row.signer_address)
                .expect("signer_address should be a valid address");
            let sender_id = escrow_accounts_snapshot
                .get_sender_for_signer(&signer_id)
                .expect("should be able to get sender from signer");

            // Accumulate allocations for the sender
            unfinalized_sender_allocations_map
                .entry(sender_id)
                .or_default()
                .extend(allocation_ids);
        }

        let nonfinal_ravs_sender_allocations_in_db = sqlx::query!(
            r#"
                SELECT DISTINCT
                    sender_address,
                    (
                        SELECT ARRAY 
                        (
                            SELECT DISTINCT allocation_id
                            FROM scalar_tap_ravs
                            WHERE sender_address = top.sender_address
                        )
                    ) AS allocation_id
                FROM scalar_tap_ravs AS top
            "#
        )
        .fetch_all(&inner.pgpool)
        .await
        .expect("should be able to fetch unfinalized RAVs from the database");

        for row in nonfinal_ravs_sender_allocations_in_db {
            let allocation_ids = row
                .allocation_id
                .expect("all RAVs should have an allocation_id")
                .iter()
                .map(|allocation_id| {
                    Address::from_str(allocation_id)
                        .expect("allocation_id should be a valid address")
                })
                .collect::<HashSet<Address>>();
            let sender_id = Address::from_str(&row.sender_address)
                .expect("sender_address should be a valid address");

            // Accumulate allocations for the sender
            unfinalized_sender_allocations_map
                .entry(sender_id)
                .or_default()
                .extend(allocation_ids);
        }

        // Create SenderAccount instances for all senders that have unfinalized allocations and add
        // the allocations to the SenderAccount instances.
        let mut sender_accounts = HashMap::new();
        for (sender_id, allocation_ids) in unfinalized_sender_allocations_map {
            let sender = sender_accounts
                .entry(sender_id)
                .or_insert(Arc::new(SenderAccount::new(
                    config,
                    inner.pgpool.clone(),
                    sender_id,
                    inner.escrow_accounts.clone(),
                    inner.escrow_subgraph,
                    inner.escrow_adapter.clone(),
                    inner.tap_eip712_domain_separator.clone(),
                    inner
                        .sender_aggregator_endpoints
                        .get(&sender_id)
                        .expect("should be able to get sender_aggregator_endpoint for sender")
                        .clone(),
                )));

            sender.update_allocations(allocation_ids).await;

            sender.recompute_unaggregated_fees().await;
        }
        // replace the sender_accounts with the updated sender_accounts
        *inner.sender_accounts.lock().unwrap() = sender_accounts;

        // Update senders and allocations based on the current state of the network.
        // It is important to do this after creating the Sender and SenderAllocation instances based
        // on the receipts in the database, because now all ineligible allocation and/or sender that
        // we created above will be set for receipt finalization.
        inner
            .update_sender_accounts(
                inner
                    .indexer_allocations
                    .value()
                    .await
                    .expect("Should get indexer allocations from Eventual"),
                escrow_accounts_snapshot.get_senders(),
            )
            .await
            .expect("Should be able to update_sender_accounts");

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(Self::new_receipts_watcher(
            pglistener,
            inner.sender_accounts.clone(),
            inner.escrow_accounts.clone(),
        ));

        // Start the eligible_allocations_senders_pipe that watches for changes in eligible senders
        // and allocations and updates the SenderAccount instances accordingly.
        let inner_clone = inner.clone();
        let eligible_allocations_senders_pipe = eventuals::join((
            inner.indexer_allocations.clone(),
            inner.escrow_accounts.clone(),
        ))
        .pipe_async(move |(indexer_allocations, escrow_accounts)| {
            let inner = inner_clone.clone();
            async move {
                inner
                    .update_sender_accounts(indexer_allocations, escrow_accounts.get_senders())
                    .await
                    .unwrap_or_else(|e| {
                        error!("Error while updating sender_accounts: {:?}", e);
                    });
            }
        });

        Self {
            _inner: inner,
            new_receipts_watcher_handle,
            _eligible_allocations_senders_pipe: eligible_allocations_senders_pipe,
        }
    }

    /// Continuously listens for new receipt notifications from Postgres and forwards them to the
    /// corresponding SenderAccount.
    async fn new_receipts_watcher(
        mut pglistener: PgListener,
        sender_accounts: Arc<StdMutex<HashMap<Address, Arc<SenderAccount>>>>,
        escrow_accounts: Eventual<EscrowAccounts>,
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

            let sender_address = escrow_accounts
                .value()
                .await
                .expect("should be able to get escrow accounts")
                .get_sender_for_signer(&new_receipt_notification.signer_address);

            let sender_address = match sender_address {
                Ok(sender_address) => sender_address,
                Err(_) => {
                    error!(
                        "No sender address found for receipt signer address {}. \
                        This should not happen.",
                        new_receipt_notification.signer_address
                    );
                    // TODO: save the receipt in the failed receipts table?
                    continue;
                }
            };

            let sender_account = sender_accounts
                .lock()
                .unwrap()
                .get(&sender_address)
                .cloned();

            if let Some(sender_account) = sender_account {
                sender_account
                    .handle_new_receipt_notification(new_receipt_notification)
                    .await;
            } else {
                warn!(
                    "No sender_allocation_manager found for sender_address {} to process new \
                    receipt notification. This should not happen.",
                    sender_address
                );
            }
        }
    }
}

impl Drop for SenderAccountsManager {
    fn drop(&mut self) {
        // Abort the notification watcher on drop. Otherwise it may panic because the PgPool could
        // get dropped before. (Observed in tests)
        self.new_receipts_watcher_handle.abort();
    }
}

#[cfg(test)]
mod tests {

    use std::vec;

    use ethereum_types::U256;
    use indexer_common::{
        prelude::{AllocationStatus, SubgraphDeployment},
        subgraph_client::DeploymentDetails,
    };
    use serde_json::json;
    use thegraph::types::DeploymentId;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    use crate::tap::test_utils::{INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR};

    use super::*;

    #[sqlx::test(migrations = "../migrations")]
    async fn test_sender_account_creation_and_eol(pgpool: PgPool) {
        let config = Box::leak(Box::new(config::Cli {
            config: None,
            ethereum: config::Ethereum {
                indexer_address: INDEXER.1,
            },
            tap: config::Tap {
                rav_request_trigger_value: 100,
                rav_request_timestamp_buffer_ms: 1,
                ..Default::default()
            },
            ..Default::default()
        }));

        let (mut indexer_allocations_writer, indexer_allocations_eventual) =
            Eventual::<HashMap<Address, Allocation>>::new();
        indexer_allocations_writer.write(HashMap::new());

        let (mut escrow_accounts_writer, escrow_accounts_eventual) =
            Eventual::<EscrowAccounts>::new();
        escrow_accounts_writer.write(EscrowAccounts::default());

        // Mock escrow subgraph.
        let mock_server = MockServer::start().await;
        mock_server
            .register(
                Mock::given(method("POST"))
                    .and(body_string_contains("transactions"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({ "data": { "transactions": []}})),
                    ),
            )
            .await;
        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
        )));

        let sender_account = SenderAccountsManager::new(
            config,
            pgpool.clone(),
            indexer_allocations_eventual,
            escrow_accounts_eventual,
            escrow_subgraph,
            TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            HashMap::from([(SENDER.1, String::from("http://localhost:8000"))]),
        )
        .await;

        let allocation_id =
            Address::from_str("0xdd975e30aafebb143e54d215db8a3e8fd916a701").unwrap();

        // Add an allocation to the indexer_allocations Eventual.
        indexer_allocations_writer.write(HashMap::from([(
            allocation_id,
            Allocation {
                id: allocation_id,
                indexer: INDEXER.1,
                allocated_tokens: U256::from_str("601726452999999979510903").unwrap(),
                created_at_block_hash:
                    "0x99d3fbdc0105f7ccc0cd5bb287b82657fe92db4ea8fb58242dafb90b1c6e2adf".to_string(),
                created_at_epoch: 953,
                closed_at_epoch: None,
                subgraph_deployment: SubgraphDeployment {
                    id: DeploymentId::from_str(
                        "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf"
                    ).unwrap(),
                    denied_at: Some(0),
                },
                status: AllocationStatus::Null,
                closed_at_epoch_start_block_hash: None,
                previous_epoch_start_block_hash: None,
                poi: None,
                query_fee_rebates: None,
                query_fees_collected: None,
            },
        )]));

        // Add an escrow account to the escrow_accounts Eventual.
        escrow_accounts_writer.write(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        // Wait for the SenderAccount to be created.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check that the SenderAccount was created.
        assert!(sender_account
            ._inner
            .sender_accounts
            .lock()
            .unwrap()
            .contains_key(&SENDER.1));

        // Remove the escrow account from the escrow_accounts Eventual.
        escrow_accounts_writer.write(EscrowAccounts::default());

        // Wait a bit
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check that the Sender's allocation moved from active to ineligible.
        assert!(sender_account
            ._inner
            .sender_accounts
            .lock()
            .unwrap()
            .get(&SENDER.1)
            .unwrap()
            ._tests_get_allocations_active()
            .is_empty());
        assert!(sender_account
            ._inner
            .sender_accounts
            .lock()
            .unwrap()
            .get(&SENDER.1)
            .unwrap()
            ._tests_get_allocations_ineligible()
            .contains_key(&allocation_id));
    }
}
