// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#![allow(missing_docs)]
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::LazyLock,
    time::Duration,
};

use actors::TestableActor;
use anyhow::anyhow;
use bigdecimal::num_bigint::BigInt;
use indexer_config;
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use indexer_receipt::TapReceipt;
use ractor::{concurrency::JoinHandle, Actor, ActorRef};
use rand::{distr::Alphanumeric, rng, Rng};
use reqwest::Url;
use sqlx::{types::BigDecimal, PgPool};
use tap_aggregator::server::run_server;
use tap_core::{signed_message::Eip712SignedMessage, tap_eip712_domain};
use tap_graph::{Receipt, ReceiptAggregateVoucher, SignedRav, SignedReceipt};
use test_assets::{flush_messages, TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
use thegraph_core::alloy::{
    primitives::{hex::ToHexExt, Address, Bytes, FixedBytes, U256},
    signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    sol_types::Eip712Domain,
};

pub const ALLOCATION_ID_0: Address = test_assets::ALLOCATION_ID_0;
pub const ALLOCATION_ID_1: Address = test_assets::ALLOCATION_ID_1;
use tokio::sync::{
    mpsc,
    watch::{self, Sender},
};
use tracing::error;

use crate::{
    agent::{
        sender_account::{
            SenderAccount, SenderAccountArgs, SenderAccountConfig, SenderAccountMessage,
        },
        sender_accounts_manager::{
            AllocationId, SenderAccountsManager, SenderAccountsManagerArgs,
            SenderAccountsManagerMessage, SenderType,
        },
    },
    tap::{
        context::{AdapterError, Horizon, Legacy, NetworkVersion},
        CheckingReceipt,
    },
};

// pub static ref SENDER: (PrivateKeySigner, Address) = wallet(0);
pub static SENDER_2: LazyLock<(PrivateKeySigner, Address)> = LazyLock::new(|| wallet(1));
pub static INDEXER: LazyLock<(PrivateKeySigner, Address)> = LazyLock::new(|| wallet(3));
pub static TAP_EIP712_DOMAIN_SEPARATOR: LazyLock<Eip712Domain> =
    LazyLock::new(|| tap_eip712_domain(1, Address::from([0x11u8; 20]), tap_core::TapVersion::V1));
pub static TAP_EIP712_DOMAIN_SEPARATOR_V2: LazyLock<Eip712Domain> =
    LazyLock::new(|| tap_eip712_domain(1, Address::from([0x11u8; 20]), tap_core::TapVersion::V2));

pub static SUBGRAPH_SERVICE_ADDRESS: [u8; 20] = [0x11u8; 20];

pub const TRIGGER_VALUE: u128 = 500;
pub const RECEIPT_LIMIT: u64 = 10000;
pub const DUMMY_URL: &str = "http://localhost:1234";
pub const ESCROW_VALUE: u128 = 1000;
const BUFFER_DURATION: Duration = Duration::from_millis(100);
const RETRY_DURATION: Duration = Duration::from_millis(1000);
const RAV_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const TAP_SENDER_TIMEOUT: Duration = Duration::from_secs(30);

const RAV_REQUEST_BUFFER: Duration = Duration::from_secs(60);
const ESCROW_POLLING_INTERVAL: Duration = Duration::from_secs(30);

/// Generates a random prefix to be used for actor registry
pub fn generate_random_prefix() -> String {
    const SIZE: usize = 16;
    rng()
        .sample_iter(&Alphanumeric)
        .take(SIZE)
        .map(char::from)
        .collect()
}

pub fn get_sender_account_config() -> &'static SenderAccountConfig {
    Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: RAV_REQUEST_BUFFER,
        max_amount_willing_to_lose_grt: TRIGGER_VALUE + 100,
        trigger_value: TRIGGER_VALUE,
        rav_request_timeout: Duration::from_secs(30),
        rav_request_receipt_limit: 1000,
        indexer_address: INDEXER.1,
        escrow_polling_interval: ESCROW_POLLING_INTERVAL,
        tap_sender_timeout: Duration::from_secs(63),
        trusted_senders: HashSet::new(),
        tap_mode: indexer_config::TapMode::Legacy,
        allocation_reconciliation_interval: Duration::from_secs(300),
    }))
}

#[allow(clippy::too_many_arguments)]
#[bon::builder]
pub async fn create_sender_account(
    pgpool: PgPool,
    #[builder(default = HashSet::new())] initial_allocation: HashSet<AllocationId>,
    #[builder(default = TRIGGER_VALUE)] rav_request_trigger_value: u128,
    #[builder(default = TRIGGER_VALUE)] max_amount_willing_to_lose_grt: u128,
    escrow_subgraph_endpoint: Option<&str>,
    network_subgraph_endpoint: Option<&str>,
    #[builder(default = RECEIPT_LIMIT)] rav_request_receipt_limit: u64,
    aggregator_endpoint: Option<Url>,
    #[builder(default = false)] trusted_sender: bool,
    #[builder(default = Duration::from_secs(300))] allocation_reconciliation_interval: Duration,
    /// Optional sender ID for test isolation. Defaults to SENDER.1.
    sender_id: Option<Address>,
) -> (
    ActorRef<SenderAccountMessage>,
    mpsc::Receiver<SenderAccountMessage>,
    String,
    Sender<EscrowAccounts>,
    Sender<HashSet<AllocationId>>,
    Address,
) {
    let sender_id = sender_id.unwrap_or(SENDER.1);
    let trusted_senders = if trusted_sender {
        HashSet::from([sender_id])
    } else {
        HashSet::new()
    };
    let config = Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: BUFFER_DURATION,
        max_amount_willing_to_lose_grt,
        trigger_value: rav_request_trigger_value,
        rav_request_timeout: RAV_REQUEST_TIMEOUT,
        rav_request_receipt_limit,
        indexer_address: INDEXER.1,
        escrow_polling_interval: Duration::default(),
        tap_sender_timeout: TAP_SENDER_TIMEOUT,
        trusted_senders,
        tap_mode: indexer_config::TapMode::Legacy,
        allocation_reconciliation_interval,
    }));

    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(network_subgraph_endpoint.unwrap_or(DUMMY_URL))
                .unwrap(),
        )
        .await,
    ));
    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint.unwrap_or(DUMMY_URL))
                .unwrap(),
        )
        .await,
    ));
    let (escrow_accounts_tx, escrow_accounts_rx) = watch::channel(EscrowAccounts::default());
    escrow_accounts_tx
        .send(EscrowAccounts::new(
            HashMap::from([(sender_id, U256::from(ESCROW_VALUE))]),
            HashMap::from([(sender_id, vec![SIGNER.1])]),
        ))
        .expect("Failed to update escrow_accounts channel");

    let prefix = generate_random_prefix();

    let aggregator_url = match aggregator_endpoint {
        Some(url) => url,
        None => Url::parse(&get_grpc_url().await).unwrap(),
    };

    let (indexer_allocations_tx, indexer_allocations_rx) = watch::channel(initial_allocation);

    let args = SenderAccountArgs {
        config,
        pgpool,
        sender_id,
        escrow_accounts: escrow_accounts_rx,
        indexer_allocations: indexer_allocations_rx,
        escrow_subgraph,
        network_subgraph,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        domain_separator_v2: TAP_EIP712_DOMAIN_SEPARATOR_V2.clone(),
        sender_aggregator_endpoint: aggregator_url,
        allocation_ids: HashSet::new(),
        prefix: Some(prefix.clone()),
        retry_interval: RETRY_DURATION,
        sender_type: SenderType::Legacy,
    };

    let (sender, mut receiver) = mpsc::channel(100);
    let actor = TestableActor::new(SenderAccount, sender);

    let (sender, _) = Actor::spawn(Some(prefix.clone()), actor, args)
        .await
        .unwrap();

    // flush all messages
    flush_messages(&mut receiver).await;

    (
        sender,
        receiver,
        prefix,
        escrow_accounts_tx,
        indexer_allocations_tx,
        sender_id,
    )
}

#[bon::builder]
pub async fn create_sender_accounts_manager(
    pgpool: PgPool,
    network_subgraph: Option<&str>,
    escrow_subgraph: Option<&str>,
    initial_escrow_accounts_v1: Option<EscrowAccounts>,
    initial_escrow_accounts_v2: Option<EscrowAccounts>,
) -> (
    String,
    mpsc::Receiver<SenderAccountsManagerMessage>,
    (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>),
) {
    let config = get_sender_account_config();
    let (_allocations_tx, allocations_rx) = watch::channel(HashMap::new());
    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph.unwrap_or(DUMMY_URL)).unwrap(),
        )
        .await,
    ));
    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(network_subgraph.unwrap_or(DUMMY_URL)).unwrap(),
        )
        .await,
    ));
    let (escrow_accounts_tx, escrow_accounts_rx) = watch::channel(EscrowAccounts::default());
    if let Some(escrow_acccounts) = initial_escrow_accounts_v1 {
        escrow_accounts_tx
            .send(escrow_acccounts)
            .expect("Failed to update escrow_accounts channel");
    }

    let (escrow_accounts_tx_v2, escrow_accounts_rx_v2) = watch::channel(EscrowAccounts::default());
    if let Some(escrow_acccounts) = initial_escrow_accounts_v2 {
        escrow_accounts_tx_v2
            .send(escrow_acccounts)
            .expect("Failed to update escrow_accounts channel");
    }

    let prefix = generate_random_prefix();
    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        domain_separator_v2: TAP_EIP712_DOMAIN_SEPARATOR_V2.clone(),
        pgpool,
        indexer_allocations: allocations_rx,
        escrow_accounts_v1: escrow_accounts_rx,
        escrow_accounts_v2: escrow_accounts_rx_v2,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: HashMap::from([
            (SENDER.1, Url::parse(&get_grpc_url().await).unwrap()),
            (SENDER_2.1, Url::parse("http://localhost:8000").unwrap()),
        ]),
        prefix: Some(prefix.clone()),
    };
    let (sender, receiver) = mpsc::channel(100);
    let actor = TestableActor::new(SenderAccountsManager, sender);
    (
        prefix,
        receiver,
        Actor::spawn(None, actor, args).await.unwrap(),
    )
}

/// Network-version specific RAV creation
pub trait CreateRav: NetworkVersion {
    fn create_rav(
        id: Address,
        signer_wallet: PrivateKeySigner,
        timestamp_ns: u64,
        value_aggregate: u128,
    ) -> Eip712SignedMessage<Self::Rav>;
}

impl CreateRav for Legacy {
    fn create_rav(
        allocation_id: Address,
        signer_wallet: PrivateKeySigner,
        timestamp_ns: u64,
        value_aggregate: u128,
    ) -> Eip712SignedMessage<Self::Rav> {
        create_rav(allocation_id, signer_wallet, timestamp_ns, value_aggregate)
    }
}

impl CreateRav for Horizon {
    fn create_rav(
        allocation_id: Address,
        signer_wallet: PrivateKeySigner,
        timestamp_ns: u64,
        value_aggregate: u128,
    ) -> Eip712SignedMessage<Self::Rav> {
        use thegraph_core::CollectionId;
        let collection_id = *CollectionId::from(allocation_id);
        create_rav_v2(collection_id, signer_wallet, timestamp_ns, value_aggregate)
    }
}

/// Fixture to generate a RAV using the wallet from `keys()`
pub fn create_rav(
    allocation_id: Address,
    signer_wallet: PrivateKeySigner,
    timestamp_ns: u64,
    value_aggregate: u128,
) -> SignedRav {
    Eip712SignedMessage::new(
        &TAP_EIP712_DOMAIN_SEPARATOR,
        ReceiptAggregateVoucher {
            allocationId: allocation_id,
            timestampNs: timestamp_ns,
            valueAggregate: value_aggregate,
        },
        &signer_wallet,
    )
    .unwrap()
}

/// Fixture to generate a RAV using the wallet from `keys()`
pub fn create_rav_v2(
    collection_id: FixedBytes<32>,
    signer_wallet: PrivateKeySigner,
    timestamp_ns: u64,
    value_aggregate: u128,
) -> tap_graph::v2::SignedRav {
    Eip712SignedMessage::new(
        &TAP_EIP712_DOMAIN_SEPARATOR,
        tap_graph::v2::ReceiptAggregateVoucher {
            collectionId: collection_id,
            timestampNs: timestamp_ns,
            valueAggregate: value_aggregate,
            payer: SENDER.1,
            dataService: SENDER.1, // Use TAP_SENDER address to match context query
            serviceProvider: INDEXER.1,
            metadata: Bytes::new(),
        },
        &signer_wallet,
    )
    .unwrap()
}

/// Network-version specific receipt creation
pub trait CreateReceipt {
    type Id: Clone + std::fmt::Debug;

    fn create_received_receipt(
        id: Self::Id,
        signer_wallet: &PrivateKeySigner,
        nonce: u64,
        timestamp_ns: u64,
        value: u128,
    ) -> CheckingReceipt;
}

impl CreateReceipt for Horizon {
    type Id = Address;

    fn create_received_receipt(
        allocation_id: Self::Id,
        signer_wallet: &PrivateKeySigner,
        nonce: u64,
        timestamp_ns: u64,
        value: u128,
    ) -> CheckingReceipt {
        use thegraph_core::CollectionId;
        let collection_id = *CollectionId::from(allocation_id);
        let receipt = Eip712SignedMessage::new(
            &TAP_EIP712_DOMAIN_SEPARATOR,
            tap_graph::v2::Receipt {
                collection_id,
                payer: SENDER.1,
                service_provider: INDEXER.1,
                data_service: SENDER.1, // Use TAP_SENDER address to match context query
                nonce,
                timestamp_ns,
                value,
            },
            signer_wallet,
        )
        .unwrap();
        CheckingReceipt::new(indexer_receipt::TapReceipt::V2(receipt))
    }
}

impl CreateReceipt for Legacy {
    type Id = Address;

    fn create_received_receipt(
        allocation_id: Self::Id,
        signer_wallet: &PrivateKeySigner,
        nonce: u64,
        timestamp_ns: u64,
        value: u128,
    ) -> CheckingReceipt {
        let receipt = Eip712SignedMessage::new(
            &TAP_EIP712_DOMAIN_SEPARATOR,
            Receipt {
                allocation_id,
                nonce,
                timestamp_ns,
                value,
            },
            signer_wallet,
        )
        .unwrap();
        CheckingReceipt::new(indexer_receipt::TapReceipt::V1(receipt))
    }
}

/// Fixture to generate a signed receipt using the wallet from `keys()` and the
/// given `query_id` and `value`
pub fn create_received_receipt(
    allocation_id: &Address,
    signer_wallet: &PrivateKeySigner,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
) -> CheckingReceipt {
    let receipt = Eip712SignedMessage::new(
        &TAP_EIP712_DOMAIN_SEPARATOR,
        Receipt {
            allocation_id: *allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        signer_wallet,
    )
    .unwrap();
    CheckingReceipt::new(indexer_receipt::TapReceipt::V1(receipt))
}

pub async fn store_receipt(pgpool: &PgPool, signed_receipt: &TapReceipt) -> anyhow::Result<u64> {
    match signed_receipt {
        TapReceipt::V1(signed_receipt) => store_receipt_v1(pgpool, signed_receipt).await,
        TapReceipt::V2(signed_receipt) => store_receipt_v2(pgpool, signed_receipt).await,
    }
}

pub async fn store_receipt_v1(
    pgpool: &PgPool,
    signed_receipt: &SignedReceipt,
) -> anyhow::Result<u64> {
    let encoded_signature = signed_receipt.signature.as_bytes().to_vec();

    let signer = signed_receipt
        .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
        .unwrap()
        .encode_hex();

    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#,
        signer,
        encoded_signature,
        signed_receipt.message.allocation_id.encode_hex(),
        BigDecimal::from(signed_receipt.message.timestamp_ns),
        BigDecimal::from(signed_receipt.message.nonce),
        BigDecimal::from(BigInt::from(signed_receipt.message.value)),
    )
    .fetch_one(pgpool)
    .await?;

    // id is BIGSERIAL, so it should be safe to cast to u64.
    let id: u64 = record.id.try_into()?;
    Ok(id)
}

pub async fn store_receipt_v2(
    pgpool: &PgPool,
    signed_receipt: &tap_graph::v2::SignedReceipt,
) -> anyhow::Result<u64> {
    let encoded_signature = signed_receipt.signature.as_bytes().to_vec();

    let signer = signed_receipt
        .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
        .unwrap()
        .encode_hex();

    let record = sqlx::query!(
        r#"
            INSERT INTO tap_horizon_receipts (
                signer_address,
                signature,
                collection_id,
                payer,
                data_service,
                service_provider,
                timestamp_ns,
                nonce,
                value
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id
        "#,
        signer,
        encoded_signature,
        signed_receipt.message.collection_id.encode_hex(),
        signed_receipt.message.payer.encode_hex(),
        signed_receipt.message.data_service.encode_hex(),
        signed_receipt.message.service_provider.encode_hex(),
        BigDecimal::from(signed_receipt.message.timestamp_ns),
        BigDecimal::from(signed_receipt.message.nonce),
        BigDecimal::from(BigInt::from(signed_receipt.message.value)),
    )
    .fetch_one(pgpool)
    .await?;

    // id is BIGSERIAL, so it should be safe to cast to u64.
    let id: u64 = record.id.try_into()?;
    Ok(id)
}

pub async fn store_batch_receipts(
    pgpool: &PgPool,
    receipts: Vec<CheckingReceipt>,
) -> Result<(), AdapterError> {
    let receipts_len = receipts.len();
    let mut signers = Vec::with_capacity(receipts_len);
    let mut signatures = Vec::with_capacity(receipts_len);
    let mut allocation_ids = Vec::with_capacity(receipts_len);
    let mut timestamps = Vec::with_capacity(receipts_len);
    let mut nonces = Vec::with_capacity(receipts_len);
    let mut values = Vec::with_capacity(receipts_len);

    for receipt in receipts {
        match receipt.signed_receipt() {
            TapReceipt::V1(receipt) => {
                signers.push(
                    receipt
                        .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                        .unwrap()
                        .encode_hex(),
                );
                signatures.push(receipt.signature.as_bytes().to_vec());
                allocation_ids.push(receipt.message.allocation_id.encode_hex().to_string());
                timestamps.push(BigDecimal::from(receipt.message.timestamp_ns));
                nonces.push(BigDecimal::from(receipt.message.nonce));
                values.push(BigDecimal::from(receipt.message.value));
            }
            TapReceipt::V2(receipt) => {
                use thegraph_core::{AllocationId, CollectionId};
                // For V2, store collection_id in the allocation_id field (as per the database reuse strategy)
                let collection_id_as_allocation =
                    AllocationId::from(CollectionId::from(receipt.message.collection_id))
                        .into_inner();
                signers.push(
                    receipt
                        .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                        .unwrap()
                        .encode_hex(),
                );
                signatures.push(receipt.signature.as_bytes().to_vec());
                allocation_ids.push(collection_id_as_allocation.encode_hex().to_string());
                timestamps.push(BigDecimal::from(receipt.message.timestamp_ns));
                nonces.push(BigDecimal::from(receipt.message.nonce));
                values.push(BigDecimal::from(receipt.message.value));
            }
        };
    }
    let _ = sqlx::query!(
        r#"INSERT INTO scalar_tap_receipts (
                signer_address,
                signature,
                allocation_id,
                timestamp_ns,
                nonce,
                value
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(40)[],
                $4::NUMERIC(20)[],
                $5::NUMERIC(20)[],
                $6::NUMERIC(40)[]
            )"#,
        &signers,
        &signatures,
        &allocation_ids,
        &timestamps,
        &nonces,
        &values,
    )
    .execute(pgpool)
    .await
    .map_err(|e| {
        error!("Failed to store receipt: {}", e);
        anyhow!(e)
    });
    Ok(())
}

pub async fn store_invalid_receipt(
    pgpool: &PgPool,
    signed_receipt: &TapReceipt,
) -> anyhow::Result<u64> {
    match signed_receipt {
        TapReceipt::V1(signed_receipt) => store_invalid_receipt_v1(pgpool, signed_receipt).await,
        TapReceipt::V2(signed_receipt) => store_invalid_receipt_v2(pgpool, signed_receipt).await,
    }
}

pub async fn store_invalid_receipt_v1(
    pgpool: &PgPool,
    signed_receipt: &SignedReceipt,
) -> anyhow::Result<u64> {
    let encoded_signature = signed_receipt.signature.as_bytes().to_vec();

    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts_invalid (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#,
        signed_receipt
            .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
            .unwrap()
            .encode_hex(),
        encoded_signature,
        signed_receipt.message.allocation_id.encode_hex(),
        BigDecimal::from(signed_receipt.message.timestamp_ns),
        BigDecimal::from(signed_receipt.message.nonce),
        BigDecimal::from(BigInt::from(signed_receipt.message.value)),
    )
    .fetch_one(pgpool)
    .await?;

    // id is BIGSERIAL, so it should be safe to cast to u64.
    let id: u64 = record.id.try_into()?;
    Ok(id)
}

pub async fn store_invalid_receipt_v2(
    pgpool: &PgPool,
    signed_receipt: &tap_graph::v2::SignedReceipt,
) -> anyhow::Result<u64> {
    use thegraph_core::{AllocationId, CollectionId};
    let encoded_signature = signed_receipt.signature.as_bytes().to_vec();

    // Store collection_id in allocation_id field (database reuse strategy)
    let collection_id_as_allocation =
        AllocationId::from(CollectionId::from(signed_receipt.message.collection_id)).into_inner();

    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts_invalid (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#,
        signed_receipt
            .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
            .unwrap()
            .encode_hex(),
        encoded_signature,
        collection_id_as_allocation.encode_hex(),
        BigDecimal::from(signed_receipt.message.timestamp_ns),
        BigDecimal::from(signed_receipt.message.nonce),
        BigDecimal::from(BigInt::from(signed_receipt.message.value)),
    )
    .fetch_one(pgpool)
    .await?;

    // id is BIGSERIAL, so it should be safe to cast to u64.
    let id: u64 = record.id.try_into()?;
    Ok(id)
}

/// Fixture to generate a wallet and address
pub fn wallet(index: u32) -> (PrivateKeySigner, Address) {
    let wallet: PrivateKeySigner= MnemonicBuilder::<English>::default()
        .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
        .index(index)
        .unwrap()
        .build()
        .unwrap();
    let address = wallet.address();
    (wallet, address)
}

pub async fn store_rav(
    pgpool: &PgPool,
    signed_rav: SignedRav,
    sender: Address,
) -> anyhow::Result<()> {
    store_rav_with_options()
        .pgpool(pgpool)
        .signed_rav(signed_rav)
        .sender(sender)
        .last(false)
        .final_rav(false)
        .call()
        .await
}

// TODO use static and check for possible errors with connection refused
pub async fn get_grpc_url() -> String {
    let (_, addr) = create_grpc_aggregator().await;
    format!("http://{addr}")
}

/// Function to start a aggregator server for testing
async fn create_grpc_aggregator() -> (JoinHandle<()>, SocketAddr) {
    let wallet = SIGNER.0.clone();
    let accepted_addresses = vec![SIGNER.1].into_iter().collect();
    let domain_separator = TAP_EIP712_DOMAIN_SEPARATOR.clone();
    let domain_separator_v2 = TAP_EIP712_DOMAIN_SEPARATOR_V2.clone();
    let max_request_body_size = 1024 * 1024; // 1 MB
    let max_response_body_size = 1024 * 1024; // 1 MB
    let max_concurrent_connections = 255;
    let port = 0;

    run_server(
        port,
        wallet,
        accepted_addresses,
        domain_separator,
        domain_separator_v2,
        max_request_body_size,
        max_response_body_size,
        max_concurrent_connections,
        None,
    )
    .await
    .unwrap()
}

#[bon::builder]
pub async fn store_rav_with_options(
    pgpool: &PgPool,
    signed_rav: SignedRav,
    sender: Address,
    last: bool,
    final_rav: bool,
) -> anyhow::Result<()> {
    let signature_bytes = signed_rav.signature.as_bytes().to_vec();

    let _fut = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_ravs (sender_address, signature, allocation_id, timestamp_ns, value_aggregate, last, final)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
        sender.encode_hex(),
        signature_bytes,
        signed_rav.message.allocationId.encode_hex(),
        BigDecimal::from(signed_rav.message.timestampNs),
        BigDecimal::from(BigInt::from(signed_rav.message.valueAggregate)),
        last,
        final_rav,
    )
    .execute(pgpool)
    .await?;

    Ok(())
}

pub mod actors {
    use std::{fmt::Debug, sync::Arc};

    use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
    use test_assets::ALLOCATION_ID_0;
    use thegraph_core::{alloy::primitives::Address, AllocationId as AllocationIdCore};
    use tokio::sync::{mpsc, watch, Notify};

    use crate::agent::{
        sender_account::{RavInformation, ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::{AllocationId, NewReceiptNotification},
        sender_allocation::SenderAllocationMessage,
        unaggregated_receipts::UnaggregatedReceipts,
    };

    #[cfg(any(test, feature = "test"))]
    pub fn clone_rpc_reply<T>(_: &ractor::RpcReplyPort<T>) -> ractor::RpcReplyPort<T> {
        ractor::concurrency::oneshot().0.into()
    }

    pub struct DummyActor;

    impl DummyActor {
        pub async fn spawn() -> ActorRef<()> {
            Actor::spawn(None, Self, ()).await.unwrap().0
        }
    }

    #[async_trait::async_trait]
    impl Actor for DummyActor {
        type Msg = ();
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }
    }

    pub struct TestableActor<T>
    where
        T: Actor,
    {
        inner: T,
        pub sender: mpsc::Sender<T::Msg>,
    }

    impl<T> TestableActor<T>
    where
        T: Actor,
    {
        pub fn new(inner: T, sender: mpsc::Sender<T::Msg>) -> Self {
            Self { inner, sender }
        }
    }

    #[macro_export]
    macro_rules! assert_triggered {
        ($notify:expr) => {
            assert_triggered!($notify, "Expected notify to be triggered");
        };

        ($notify:expr, $msg:expr) => {
            if tokio::time::timeout(Duration::from_millis(10), $notify.notified())
                .await
                .is_err()
            {
                panic!($msg);
            }
        };
    }

    #[macro_export]
    macro_rules! assert_not_triggered {
        ($notify:expr) => {
            assert_not_triggered!($notify, "Expected notify to be not be triggered");
        };
        ($notify:expr, $msg:expr) => {
            if tokio::time::timeout(Duration::from_millis(10), $notify.notified())
                .await
                .is_ok()
            {
                panic!($msg);
            }
        };
    }

    #[async_trait::async_trait()]
    impl<T> Actor for TestableActor<T>
    where
        T: Actor,
        T::Msg: Debug + Clone,
    {
        type Msg = T::Msg;
        type State = T::State;
        type Arguments = T::Arguments;

        async fn pre_start(
            &self,
            myself: ActorRef<Self::Msg>,
            args: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            self.inner.pre_start(myself, args).await
        }

        async fn post_stop(
            &self,
            myself: ActorRef<Self::Msg>,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.inner.post_stop(myself, state).await
        }

        async fn handle(
            &self,
            myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            let message = msg.clone();
            let result = self.inner.handle(myself, msg, state).await;
            self.sender.send(message).await.expect("Channel failed");
            result
        }

        async fn handle_supervisor_evt(
            &self,
            myself: ActorRef<Self::Msg>,
            message: SupervisionEvent,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.inner
                .handle_supervisor_evt(myself, message, state)
                .await
        }
    }

    /// Mock implementation of SenderAllocation for testing purposes.
    ///
    /// This mock simulates the behavior of a real sender allocation actor, particularly
    /// for testing RAV request flows and retry mechanisms. When a RAV request is triggered,
    /// it sends back a successful response that follows TAP protocol behavior:
    ///
    /// - Clears unaggregated fees to zero (they become part of the RAV)
    /// - Creates a RAV for the full aggregated amount
    /// - Properly resolves deny conditions to stop unnecessary retries
    ///
    /// This implementation aligns with the documented expectation:
    /// "set the unnagregated fees to zero and the rav to the amount"
    pub struct MockSenderAllocation {
        triggered_rav_request: Arc<Notify>,
        sender_actor: Option<ActorRef<SenderAccountMessage>>,
        next_unaggregated_fees_value: watch::Receiver<u128>,
        receipts: mpsc::Sender<NewReceiptNotification>,
    }

    impl MockSenderAllocation {
        pub fn new_with_triggered_rav_request(
            sender_actor: ActorRef<SenderAccountMessage>,
        ) -> (Self, Arc<Notify>, watch::Sender<u128>) {
            let triggered_rav_request = Arc::new(Notify::new());
            let (unaggregated_fees, next_unaggregated_fees_value) = watch::channel(0);
            (
                Self {
                    sender_actor: Some(sender_actor),
                    triggered_rav_request: triggered_rav_request.clone(),
                    receipts: mpsc::channel(1).0,
                    next_unaggregated_fees_value,
                },
                triggered_rav_request,
                unaggregated_fees,
            )
        }

        pub fn new_with_next_rav_value(
            sender_actor: ActorRef<SenderAccountMessage>,
        ) -> (Self, watch::Sender<u128>) {
            let (unaggregated_fees, next_unaggregated_fees_value) = watch::channel(0);
            (
                Self {
                    sender_actor: Some(sender_actor),
                    triggered_rav_request: Arc::new(Notify::new()),
                    receipts: mpsc::channel(1).0,
                    next_unaggregated_fees_value,
                },
                unaggregated_fees,
            )
        }

        pub fn new_with_receipts() -> (Self, mpsc::Receiver<NewReceiptNotification>) {
            let (tx, rx) = mpsc::channel(10);

            (
                Self {
                    sender_actor: None,
                    triggered_rav_request: Arc::new(Notify::new()),
                    receipts: tx,
                    next_unaggregated_fees_value: watch::channel(0).1,
                },
                rx,
            )
        }
    }

    #[async_trait::async_trait]
    impl Actor for MockSenderAllocation {
        type Msg = SenderAllocationMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _allocation_ids: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            match message {
                SenderAllocationMessage::TriggerRavRequest => {
                    self.triggered_rav_request.notify_one();
                    if let Some(sender_account) = self.sender_actor.as_ref() {
                        // Mock a successful RAV request response that follows TAP protocol behavior:
                        // 1. Aggregate unaggregated receipts into a Receipt Aggregate Voucher (RAV)
                        // 2. Clear unaggregated fees to zero (they're now represented in the RAV)
                        // 3. Create a RAV for the full aggregated amount
                        //
                        // This behavior aligns with the documented expectation:
                        // "set the unnagregated fees to zero and the rav to the amount"
                        // (see sender_account.rs test_deny_allow comment)
                        //
                        // Important: This correctly resolves the deny condition when unaggregated
                        // fees are cleared, which stops the retry mechanism as intended.
                        let current_value = *self.next_unaggregated_fees_value.borrow();
                        sender_account.cast(SenderAccountMessage::UpdateReceiptFees(
                            AllocationId::Legacy(AllocationIdCore::from(ALLOCATION_ID_0)),
                            ReceiptFees::RavRequestResponse(
                                UnaggregatedReceipts {
                                    value: 0, // Clear unaggregated fees - they're now in the RAV
                                    last_id: 0,
                                    counter: 0,
                                },
                                Ok(Some(RavInformation {
                                    allocation_id: ALLOCATION_ID_0,
                                    value_aggregate: current_value, // RAV for the full amount
                                })),
                            ),
                        ))?;
                    }
                }
                SenderAllocationMessage::NewReceipt(receipt) => {
                    self.receipts.send(receipt).await.unwrap();
                }
                _ => {}
            }
            Ok(())
        }
    }

    pub async fn create_mock_sender_allocation(
        prefix: String,
        sender: Address,
        allocation: Address,
        sender_actor: ActorRef<SenderAccountMessage>,
    ) -> (
        Arc<Notify>,
        watch::Sender<u128>,
        ActorRef<SenderAllocationMessage>,
    ) {
        let (mock_sender_allocation, triggered_rav_request, next_unaggregated_fees) =
            MockSenderAllocation::new_with_triggered_rav_request(sender_actor);

        let name = format!("{prefix}:{sender}:{allocation}");
        let (sender_account, _) =
            MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
                .await
                .unwrap();
        (
            triggered_rav_request,
            next_unaggregated_fees,
            sender_account,
        )
    }

    pub struct MockSenderAccount {
        pub last_message_emitted: mpsc::Sender<SenderAccountMessage>,
    }

    #[async_trait::async_trait]
    impl Actor for MockSenderAccount {
        type Msg = SenderAccountMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _allocation_ids: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.last_message_emitted.send(message).await.unwrap();
            Ok(())
        }
    }

    pub async fn create_mock_sender_account() -> (
        mpsc::Receiver<SenderAccountMessage>,
        ActorRef<SenderAccountMessage>,
    ) {
        let (last_message_emitted, rx) = mpsc::channel(64);

        let (sender_account, _) = MockSenderAccount::spawn(
            None,
            MockSenderAccount {
                last_message_emitted: last_message_emitted.clone(),
            },
            (),
        )
        .await
        .unwrap();
        (rx, sender_account)
    }
}
