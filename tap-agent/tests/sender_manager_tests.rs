// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, str::FromStr, vec};

use alloy_primitives::Address;
use ethereum_types::U256;
use eventuals::Eventual;
use indexer_common::{
    allocations::Allocation,
    escrow_accounts::EscrowAccounts,
    prelude::{AllocationStatus, SubgraphDeployment},
    subgraph_client::{DeploymentDetails, SubgraphClient},
};
use indexer_tap_agent::{
    agent::{
        sender_account::SenderAccountMessage,
        sender_accounts_manager::{SenderAccountsManager, SenderAccountsManagerArgs},
        sender_allocation::SenderAllocationMessage,
    },
    config,
};
use ractor::{Actor, ActorRef, ActorStatus};
use serde_json::json;
use sqlx::PgPool;
use thegraph::types::DeploymentId;
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

mod test_utils;
use test_utils::{INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR};

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

    let (mut escrow_accounts_writer, escrow_accounts_eventual) = Eventual::<EscrowAccounts>::new();
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

    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        pgpool: pgpool.clone(),
        indexer_allocations: indexer_allocations_eventual,
        escrow_accounts: escrow_accounts_eventual,
        escrow_subgraph,
        sender_aggregator_endpoints: HashMap::from([(
            SENDER.1,
            String::from("http://localhost:8000"),
        )]),
    };
    SenderAccountsManager::spawn(None, SenderAccountsManager, args)
        .await
        .unwrap();

    let allocation_id = Address::from_str("0xdd975e30aafebb143e54d215db8a3e8fd916a701").unwrap();

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
                    "0xcda7fa0405d6fd10721ed13d18823d24b535060d8ff661f862b26c23334f13bf",
                )
                .unwrap(),
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
    let sender_account = ActorRef::<SenderAccountMessage>::where_is(SENDER.1.to_string()).unwrap();
    assert_eq!(sender_account.get_status(), ActorStatus::Running);

    let sender_allocation_id = format!("{}:{}", SENDER.1, allocation_id);
    let sender_allocation =
        ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id).unwrap();
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
