// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::{collections::HashMap, str::FromStr};

use crate::agent::sender_allocation::SenderAllocationMsg;
use alloy_sol_types::Eip712Domain;
use anyhow::anyhow;
use anyhow::Result;
use eventuals::{Eventual, EventualExt, PipeHandle};
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::prelude::{Allocation, SubgraphClient};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use thegraph::types::Address;
use tracing::{error, warn};

use super::sender_account::{SenderAccount, SenderAccountMessage};
use crate::config;

#[derive(Deserialize, Debug)]
pub struct NewReceiptNotification {
    pub id: u64,
    pub allocation_id: Address,
    pub signer_address: Address,
    pub timestamp_ns: u64,
    pub value: u128,
}

pub struct SenderAccountsManager {
    config: &'static config::Cli,
    pgpool: PgPool,
    indexer_allocations: Eventual<HashSet<Address>>,
    escrow_accounts: Eventual<EscrowAccounts>,
    escrow_subgraph: &'static SubgraphClient,
    tap_eip712_domain_separator: Eip712Domain,
    sender_aggregator_endpoints: HashMap<Address, String>,
}

pub enum NetworkMessage {
    UpdateSenderAccounts(HashSet<Address>),
    CreateSenderAccount(Address, HashSet<Address>),
}

pub struct State {
    sender_ids: HashSet<Address>,
    new_receipts_watcher_handle: tokio::task::JoinHandle<()>,
    _eligible_allocations_senders_pipe: PipeHandle,
}

#[async_trait::async_trait]
impl Actor for SenderAccountsManager {
    type Msg = NetworkMessage;
    type State = State;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        let mut pglistener = PgListener::connect_with(&self.pgpool.clone())
            .await
            .unwrap();
        pglistener
            .listen("scalar_tap_receipt_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
            );
        // Start the new_receipts_watcher task that will consume from the `pglistener`
        let new_receipts_watcher_handle = tokio::spawn(Self::new_receipts_watcher(
            pglistener,
            self.escrow_accounts.clone(),
        ));
        let clone = myself.clone();
        let _eligible_allocations_senders_pipe =
            self.escrow_accounts
                .clone()
                .pipe_async(move |escrow_accounts| {
                    let myself = clone.clone();

                    async move {
                        myself
                            .cast(NetworkMessage::UpdateSenderAccounts(
                                escrow_accounts.get_senders(),
                            ))
                            .unwrap_or_else(|e| {
                                error!("Error while updating sender_accounts: {:?}", e);
                            });
                    }
                });
        let sender_allocation = self.get_pending_sender_allocation_id().await;
        for (sender_id, allocation_ids) in sender_allocation {
            myself.cast(NetworkMessage::CreateSenderAccount(
                sender_id,
                allocation_ids,
            ))?;
        }

        Ok(State {
            sender_ids: HashSet::new(),
            new_receipts_watcher_handle,
            _eligible_allocations_senders_pipe,
        })
    }
    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        // Abort the notification watcher on drop. Otherwise it may panic because the PgPool could
        // get dropped before. (Observed in tests)
        state.new_receipts_watcher_handle.abort();
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match msg {
            NetworkMessage::UpdateSenderAccounts(target_senders) => {
                // Create new sender accounts
                for sender in target_senders.difference(&state.sender_ids) {
                    myself.cast(NetworkMessage::CreateSenderAccount(*sender, HashSet::new()))?;
                }

                // Remove sender accounts
                for sender in state.sender_ids.difference(&target_senders) {
                    if let Some(sender_handle) =
                        ActorRef::<SenderAccountMessage>::where_is(sender.to_string())
                    {
                        sender_handle.cast(SenderAccountMessage::RemoveSenderAccount)?;
                    }
                }

                state.sender_ids = target_senders;
            }
            NetworkMessage::CreateSenderAccount(sender_id, allocation_ids) => {
                let sender_account = self.new_sender_account(&sender_id)?;
                SenderAccount::spawn_linked(
                    Some(sender_id.to_string()),
                    sender_account,
                    allocation_ids,
                    myself.get_cell(),
                )
                .await?;
            }
        }
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
        let indexer_allocations = indexer_allocations.map(|allocations| async move {
            allocations.keys().cloned().collect::<HashSet<Address>>()
        });
        Self {
            config,
            pgpool,
            indexer_allocations,
            escrow_accounts,
            escrow_subgraph,
            tap_eip712_domain_separator,
            sender_aggregator_endpoints,
        }
    }

    async fn get_pending_sender_allocation_id(&self) -> HashMap<Address, HashSet<Address>> {
        let escrow_accounts_snapshot = self
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
        .fetch_all(&self.pgpool)
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
        .fetch_all(&self.pgpool)
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
        unfinalized_sender_allocations_map
    }
    fn new_sender_account(&self, sender_id: &Address) -> Result<SenderAccount> {
        Ok(SenderAccount::new(
            self.config,
            self.pgpool.clone(),
            *sender_id,
            self.escrow_accounts.clone(),
            self.indexer_allocations.clone(),
            self.escrow_subgraph,
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
        ))
    }

    /// Continuously listens for new receipt notifications from Postgres and forwards them to the
    /// corresponding SenderAccount.
    async fn new_receipts_watcher(
        mut pglistener: PgListener,
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
            let allocation_id = &new_receipt_notification.allocation_id;

            if let Some(sender_allocation) = ActorRef::<SenderAllocationMsg>::where_is(format!(
                "{sender_address}:{allocation_id}"
            )) {
                if let Err(e) = sender_allocation
                    .cast(SenderAllocationMsg::NewReceipt(new_receipt_notification))
                {
                    error!("Error while forwarding new receipt notification to sender_allocation: {:?}", e);
                }
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

#[cfg(test)]
mod tests {

    use std::vec;

    use ethereum_types::U256;
    use indexer_common::{
        prelude::{AllocationStatus, SubgraphDeployment},
        subgraph_client::DeploymentDetails,
    };
    use ractor::ActorStatus;
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
        SenderAccountsManager::spawn(None, sender_account, ())
            .await
            .unwrap();

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
        let sender_account =
            ActorRef::<SenderAccountMessage>::where_is(SENDER.1.to_string()).unwrap();
        assert_eq!(sender_account.get_status(), ActorStatus::Running);

        let sender_allocation_id = format!("{}:{}", SENDER.1, allocation_id);
        let sender_allocation =
            ActorRef::<SenderAllocationMsg>::where_is(sender_allocation_id).unwrap();
        assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

        // Remove the escrow account from the escrow_accounts Eventual.
        escrow_accounts_writer.write(EscrowAccounts::default());

        // Wait a bit
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check that the Sender's allocation moved from active to ineligible.

        assert_eq!(sender_account.get_status(), ActorStatus::Stopped);
        assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

        let sender_account = ActorRef::<SenderAccountMessage>::where_is(SENDER.1.to_string());

        assert!(sender_account.is_none());
    }
}
