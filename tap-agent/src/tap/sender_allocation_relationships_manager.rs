// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::{collections::HashMap, str::FromStr, sync::Arc};

use alloy_primitives::Address;
use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::prelude::{Allocation, SubgraphClient};
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use tokio::sync::RwLock;
use tracing::{error, warn};

use super::escrow_adapter::EscrowAdapter;
use super::sender_allocation_relationship::SenderAllocationRelationship;
use crate::config;

#[derive(Deserialize, Debug)]
pub struct NewReceiptNotification {
    pub id: u64,
    pub allocation_id: Address,
    pub signer_address: Address,
    pub timestamp_ns: u64,
    pub value: u128,
}

pub struct SenderAllocationRelationshipsManager {
    _inner: Arc<Inner>,
    new_receipts_watcher_handle: tokio::task::JoinHandle<()>,
    _eligible_allocations_senders_pipe: PipeHandle,
}

#[derive(Clone)]
struct Inner {
    config: &'static config::Cli,
    pgpool: PgPool,
    /// Map of (allocation_id, sender_address) to SenderAllocationRelationship.
    sender_allocation_relationships:
        Arc<RwLock<HashMap<(Address, Address), SenderAllocationRelationship>>>,
    indexer_allocations: Eventual<HashMap<Address, Allocation>>,
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    escrow_adapter: EscrowAdapter,
    tap_eip712_domain_separator: Eip712Domain,
    sender_aggregator_endpoints: HashMap<Address, String>,
}

impl SenderAllocationRelationshipsManager {
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
            sender_allocation_relationships: Arc::new(RwLock::new(HashMap::new())),
            indexer_allocations,
            escrow_accounts,
            escrow_subgraph,
            escrow_adapter,
            tap_eip712_domain_separator,
            sender_aggregator_endpoints,
        });

        let escrow_accounts_snapshot = inner
            .escrow_accounts
            .value()
            .await
            .expect("Should get escrow accounts from Eventual");

        Self::update_sender_allocation_relationships(
            &inner,
            inner
                .indexer_allocations
                .value()
                .await
                .expect("Should get indexer allocations from Eventual"),
            escrow_accounts_snapshot.get_senders(),
        )
        .await
        .expect("Should be able to update sender_allocation_relationships");

        // Listen to pg_notify events. We start it before updating the unaggregated_fees for all
        // SenderAllocationRelationship instances, so that we don't miss any receipts. PG will
        // buffer the notifications until we start consuming them with `new_receipts_watcher`.
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

        let mut sender_allocation_relationships_write_lock =
            inner.sender_allocation_relationships.write().await;

        // Create SenderAllocationRelationship instances for all outstanding receipts in the
        // database, because they may be linked to allocations that are not eligible anymore, but
        // still need to get aggregated.
        sqlx::query!(
            r#"
                SELECT DISTINCT allocation_id, signer_address
                FROM scalar_tap_receipts
            "#
        )
        .fetch_all(&inner.pgpool)
        .await
        .unwrap()
        .into_iter()
        .for_each(|row| {
            let allocation_id = Address::from_str(&row.allocation_id)
                .expect("allocation_id should be a valid address");
            let signer = Address::from_str(&row.signer_address)
                .expect("signer_address should be a valid address");
            let sender = escrow_accounts_snapshot
                .get_sender_for_signer(&signer)
                .expect("should be able to get sender from signer");

            // Only create a SenderAllocationRelationship if it doesn't exist yet.
            if let std::collections::hash_map::Entry::Vacant(e) =
                sender_allocation_relationships_write_lock.entry((allocation_id, sender))
            {
                e.insert(SenderAllocationRelationship::new(
                    config,
                    inner.pgpool.clone(),
                    allocation_id,
                    sender,
                    inner.escrow_accounts.clone(),
                    inner.escrow_subgraph,
                    inner.escrow_adapter.clone(),
                    inner.tap_eip712_domain_separator.clone(),
                    inner
                        .sender_aggregator_endpoints
                        .get(&sender)
                        .unwrap()
                        .clone(),
                ));
            }
        });

        // Update the unaggregated_fees for all SenderAllocationRelationship instances by pulling
        // the receipts from the database.
        for sender_allocation_relationship in sender_allocation_relationships_write_lock.values() {
            sender_allocation_relationship
                .update_unaggregated_fees()
                .await
                .expect("should be able to update unaggregated_fees");
        }

        drop(sender_allocation_relationships_write_lock);

        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(Self::new_receipts_watcher(
            pglistener,
            inner.sender_allocation_relationships.clone(),
            inner.escrow_accounts.clone(),
        ));

        // Start the eligible_allocations_senders_pipe that watches for changes in eligible senders
        // and allocations and updates the SenderAllocationRelationship instances accordingly.
        let inner_clone = inner.clone();
        let eligible_allocations_senders_pipe = eventuals::join((
            inner.indexer_allocations.clone(),
            inner.escrow_accounts.clone(),
        ))
        .pipe_async(move |(indexer_allocations, escrow_accounts)| {
            let inner = inner_clone.clone();
            async move {
                Self::update_sender_allocation_relationships(
                    &inner,
                    indexer_allocations,
                    escrow_accounts.get_senders(),
                )
                .await
                .unwrap_or_else(|e| {
                    error!(
                        "Error while updating sender_allocation_relationships: {:?}",
                        e
                    );
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
    /// corresponding SenderAllocationRelationship.
    async fn new_receipts_watcher(
        mut pglistener: PgListener,
        sender_allocation_relationships: Arc<
            RwLock<HashMap<(Address, Address), SenderAllocationRelationship>>,
        >,
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

            if let Some(sender_allocation_relationship) = sender_allocation_relationships
                .read()
                .await
                .get(&(new_receipt_notification.allocation_id, sender_address))
            {
                sender_allocation_relationship
                    .handle_new_receipt_notification(new_receipt_notification)
                    .await;
            } else {
                warn!(
                    "No sender_allocation_relationship found for allocation_id {} and \
                    sender_address {} to process new receipt notification. This should not \
                    happen.",
                    new_receipt_notification.allocation_id, sender_address
                );
            }
        }
    }

    async fn update_sender_allocation_relationships(
        inner: &Inner,
        indexer_allocations: HashMap<Address, Allocation>,
        senders: HashSet<Address>,
    ) -> Result<()> {
        let eligible_allocations: Vec<Address> = indexer_allocations.keys().copied().collect();
        let mut sender_allocation_relationships_write =
            inner.sender_allocation_relationships.write().await;

        // Create SenderAllocationRelationship instances for all currently eligible
        // (allocation, sender)
        for allocation_id in &eligible_allocations {
            for sender in &senders {
                // Only create a SenderAllocationRelationship if it doesn't exist yet.
                if let std::collections::hash_map::Entry::Vacant(e) =
                    sender_allocation_relationships_write.entry((*allocation_id, *sender))
                {
                    e.insert(SenderAllocationRelationship::new(
                        inner.config,
                        inner.pgpool.clone(),
                        *allocation_id,
                        *sender,
                        inner.escrow_accounts.clone(),
                        inner.escrow_subgraph,
                        inner.escrow_adapter.clone(),
                        inner.tap_eip712_domain_separator.clone(),
                        inner
                            .sender_aggregator_endpoints
                            .get(sender)
                            .ok_or_else(|| {
                                anyhow!("No sender_aggregator_endpoint found for sender {}", sender)
                            })?
                            .clone(),
                    ));
                }
            }
        }

        // Trigger a last rav request for all SenderAllocationRelationship instances that correspond
        // to ineligible (allocations, sender).
        for ((allocation_id, sender), sender_allocation_relatioship) in
            sender_allocation_relationships_write.iter()
        {
            if !eligible_allocations.contains(allocation_id) || !senders.contains(sender) {
                sender_allocation_relatioship.start_last_rav_request().await
            }
        }

        // TODO: remove SenderAllocationRelationship instances that are finished. Ideally done in
        //      another async task?

        Ok(())
    }
}

impl Drop for SenderAllocationRelationshipsManager {
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

    use crate::tap::{
        sender_allocation_relationship::State,
        test_utils::{INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR},
    };

    use super::*;

    #[sqlx::test(migrations = "../migrations")]
    async fn test_sender_allocation_relatioship_creation_and_eol(pgpool: PgPool) {
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

        let sender_allocation_relatioships = SenderAllocationRelationshipsManager::new(
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
                    id: DeploymentId(
                        "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf"
                            .parse()
                            .unwrap(),
                    ),
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

        // Wait for the SenderAllocationRelationship to be created.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check that the SenderAllocationRelationship was created.
        assert!(sender_allocation_relatioships
            ._inner
            .sender_allocation_relationships
            .write()
            .await
            .contains_key(&(allocation_id, SENDER.1)));

        // Remove the escrow account from the escrow_accounts Eventual.
        escrow_accounts_writer.write(EscrowAccounts::default());

        // Wait a bit
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check that the SenderAllocationRelationship state is last_rav_pending
        assert_eq!(
            sender_allocation_relatioships
                ._inner
                .sender_allocation_relationships
                .read()
                .await
                .get(&(allocation_id, SENDER.1))
                .unwrap()
                .state()
                .await,
            State::LastRavPending
        );
    }
}
