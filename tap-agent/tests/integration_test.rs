// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
#[allow(dead_code)]
use alloy_primitives::hex::ToHex;
use alloy_primitives::Address;
use alloy_sol_types::{eip712_domain, Eip712Domain};
use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
use ethers::prelude::coins_bip39::English;
use ethers_signers::{LocalWallet, MnemonicBuilder, Signer};
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use indexer_common::subgraph_client::{DeploymentDetails, SubgraphClient};
use indexer_tap_agent::agent::sender_account::{
    SenderAccount, SenderAccountArgs, SenderAccountMessage,
};
use indexer_tap_agent::agent::sender_accounts_manager::NewReceiptNotification;
use indexer_tap_agent::agent::sender_allocation::SenderAllocationMessage;
use indexer_tap_agent::agent::unaggregated_receipts::UnaggregatedReceipts;
use indexer_tap_agent::config;
use lazy_static::lazy_static;
use ractor::{call, cast, Actor, ActorRef};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;
use std::{collections::HashMap, sync::atomic::AtomicU32};
use tap_aggregator::server::run_server;
use tap_core::{rav::ReceiptAggregateVoucher, signed_message::EIP712SignedMessage};
use tokio::task::JoinHandle;
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockServer, ResponseTemplate,
};

const DUMMY_URL: &str = "http://localhost:1234";
const VALUE_PER_RECEIPT: u64 = 100;
const TRIGGER_VALUE: u64 = 500;
static PREFIX_ID: AtomicU32 = AtomicU32::new(0);

lazy_static! {
    pub static ref ALLOCATION_ID_0: Address =
        Address::from_str("0xabababababababababababababababababababab").unwrap();
    pub static ref ALLOCATION_ID_1: Address =
        Address::from_str("0xbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc").unwrap();
    pub static ref SENDER: (LocalWallet, Address) = wallet(0);
    pub static ref SENDER_2: (LocalWallet, Address) = wallet(1);
    pub static ref SIGNER: (LocalWallet, Address) = wallet(2);
    pub static ref INDEXER: (LocalWallet, Address) = wallet(3);
    pub static ref TAP_EIP712_DOMAIN_SEPARATOR: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address:: from([0x11u8; 20]),
    };
}

pub fn wallet(index: u32) -> (LocalWallet, Address) {
    let wallet: LocalWallet = MnemonicBuilder::<English>::default()
        .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
        .index(index)
        .unwrap()
        .build()
        .unwrap();
    let address = wallet.address();
    (wallet, Address::from_slice(address.as_bytes()))
}

async fn create_aggregator() {
    // Start a TAP aggregator server.
    let (handle, aggregator_endpoint) = run_server(
        0,
        SIGNER.0.clone(),
        vec![SIGNER.1].into_iter().collect(),
        TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        100 * 1024,
        100 * 1024,
        1,
    )
    .await
    .unwrap();
}

async fn create_mock_server() {
    // Start a mock graphql server using wiremock
    let mock_server = MockServer::start().await;

    // Mock result for TAP redeem txs for (allocation, sender) pair.
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
}

async fn create_sender_accounts_manager() {}

#[sqlx::test(migrations = "../migrations")]
async fn test_full_happy_path(pgpool: PgPool) {

    // create mock server

    // create mock server for getting escrow subgraph

    // create sender_allocation_manager

    // add sender account

    // add sender allocation 1
    // add sender allocation 2
    // add sender allocation 3

    // store receipt

    // check if receipt was detected by notification handler

    // check if it was added on the sender_allocation

    // check if it was added on the sender_account

    // add more receipts on both until a trigger value is reached

    // check if it was triggered

    // check if there's a rav on the database

    // close the sender allocation 1

    // check if the rav 1 is now final

    // close the sender account

    // check if the rav 2 is now final

    // kill the manager (simulating a shutdown of the system)

    // check if the rav 3 is still non-final
}
