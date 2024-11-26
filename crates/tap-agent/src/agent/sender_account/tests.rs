
use super::{SenderAccount, SenderAccountArgs, SenderAccountMessage};
use crate::agent::sender_account::ReceiptFees;
use crate::agent::sender_accounts_manager::NewReceiptNotification;
use crate::agent::sender_allocation::SenderAllocationMessage;
use crate::agent::unaggregated_receipts::UnaggregatedReceipts;
use crate::tap::test_utils::{
    create_rav, store_rav_with_options, ALLOCATION_ID_0, ALLOCATION_ID_1, INDEXER, SENDER, SIGNER,
    TAP_EIP712_DOMAIN_SEPARATOR,
};
use alloy::hex::ToHexExt;
use alloy::primitives::{Address, U256};
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use ractor::concurrency::JoinHandle;
use ractor::{call, Actor, ActorProcessingErr, ActorRef, ActorStatus};
use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch::{self, Sender};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockGuard, MockServer, ResponseTemplate};

// we implement the PartialEq and Eq traits for SenderAccountMessage to be able to compare
impl Eq for SenderAccountMessage {}

impl PartialEq for SenderAccountMessage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::UpdateAllocationIds(l0), Self::UpdateAllocationIds(r0)) => l0 == r0,
            (Self::UpdateReceiptFees(l0, l1), Self::UpdateReceiptFees(r0, r1)) => {
                l0 == r0
                    && match (l1, r1) {
                        (ReceiptFees::NewReceipt(l1, l2), ReceiptFees::NewReceipt(r1, r2)) => {
                            r1 == l1 && r2 == l2
                        }
                        (ReceiptFees::UpdateValue(l), ReceiptFees::UpdateValue(r)) => r == l,
                        (
                            ReceiptFees::RavRequestResponse(l),
                            ReceiptFees::RavRequestResponse(r),
                        ) => match (l, r) {
                            ((fee, Ok(rav)), (fee1, Ok(rav1))) => fee == fee1 && rav == rav1,
                            ((fee, Err(error)), (fee1, Err(error1))) => {
                                fee == fee1 && error.to_string() == error1.to_string()
                            }
                            _ => false,
                        },
                        (ReceiptFees::Retry, ReceiptFees::Retry) => true,
                        _ => false,
                    }
            }
            (Self::UpdateInvalidReceiptFees(l0, l1), Self::UpdateInvalidReceiptFees(r0, r1)) => {
                l0 == r0 && l1 == r1
            }
            (Self::NewAllocationId(l0), Self::NewAllocationId(r0)) => l0 == r0,
            (a, b) => match (
                core::mem::discriminant(self),
                core::mem::discriminant(other),
            ) {
                (a, b) if a != b => false,
                _ => unimplemented!("PartialEq not implementated for {a:?} and {b:?}"),
            },
        }
    }
}

pub static PREFIX_ID: AtomicU32 = AtomicU32::new(0);
const DUMMY_URL: &str = "http://localhost:1234";
const TRIGGER_VALUE: u128 = 500;
const ESCROW_VALUE: u128 = 1000;
const BUFFER_MS: u64 = 100;
const RECEIPT_LIMIT: u64 = 10000;

async fn mock_escrow_subgraph() -> (MockServer, MockGuard) {
    let mock_ecrow_subgraph_server: MockServer = MockServer::start().await;
    let _mock_ecrow_subgraph = mock_ecrow_subgraph_server
                .register_as_scoped(
                    Mock::given(method("POST"))
                        .and(body_string_contains("TapTransactions"))
                        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                                "transactions": [{
                                    "id": "0x00224ee6ad4ae77b817b4e509dc29d644da9004ad0c44005a7f34481d421256409000000"
                                }],
                            }
                        }))),
                )
                .await;
    (mock_ecrow_subgraph_server, _mock_ecrow_subgraph)
}

async fn create_sender_account(
    pgpool: PgPool,
    initial_allocation: HashSet<Address>,
    rav_request_trigger_value: u128,
    max_amount_willing_to_lose_grt: u128,
    escrow_subgraph_endpoint: &str,
    network_subgraph_endpoint: &str,
    rav_request_receipt_limit: u64,
) -> (
    ActorRef<SenderAccountMessage>,
    tokio::task::JoinHandle<()>,
    String,
    Sender<EscrowAccounts>,
) {
    let config = Box::leak(Box::new(super::SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(BUFFER_MS),
        max_amount_willing_to_lose_grt,
        trigger_value: rav_request_trigger_value,
        rav_request_timeout: Duration::default(),
        rav_request_receipt_limit,
        indexer_address: INDEXER.1,
        escrow_polling_interval: Duration::default(),
    }));

    let network_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(network_subgraph_endpoint).unwrap(),
        )
        .await,
    ));
    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
        )
        .await,
    ));
    let (escrow_accounts_tx, escrow_accounts_rx) = watch::channel(EscrowAccounts::default());
    escrow_accounts_tx
        .send(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .expect("Failed to update escrow_accounts channel");

    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );

    let args = SenderAccountArgs {
        config,
        pgpool,
        sender_id: SENDER.1,
        escrow_accounts: escrow_accounts_rx,
        indexer_allocations: watch::channel(initial_allocation).1,
        escrow_subgraph,
        network_subgraph,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        sender_aggregator_endpoint: Url::parse(DUMMY_URL).unwrap(),
        allocation_ids: HashSet::new(),
        prefix: Some(prefix.clone()),
        retry_interval: Duration::from_millis(10),
    };

    let (sender, handle) = SenderAccount::spawn(Some(prefix.clone()), SenderAccount, args)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    (sender, handle, prefix, escrow_accounts_tx)
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_update_allocation_ids(pgpool: PgPool) {
    // Start a mock graphql server using wiremock
    let mock_server = MockServer::start().await;

    let no_allocations_closed_guard = mock_server
        .register_as_scoped(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "meta": {
                            "block": {
                                "number": 1,
                                "hash": "hash",
                                "timestamp": 1
                            }
                        },
                        "allocations": []
                    }
                }))),
        )
        .await;

    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        &mock_escrow_subgraph_server.uri(),
        &mock_server.uri(),
        RECEIPT_LIMIT,
    )
    .await;

    // we expect it to create a sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![*ALLOCATION_ID_0].into_iter().collect(),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // verify if create sender account
    let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_some());

    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_some());

    drop(no_allocations_closed_guard);
    mock_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "meta": {
                            "block": {
                                "number": 1,
                                "hash": "hash",
                                "timestamp": 1
                            }
                        },
                        "allocations": [
                            {"id": *ALLOCATION_ID_0 }
                        ]
                    }
                }))),
        )
        .await;

    // try to delete sender allocation_id
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_none());

    // safely stop the manager
    sender_account.stop_and_wait(None, None).await.unwrap();

    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_new_allocation_id(pgpool: PgPool) {
    // Start a mock graphql server using wiremock
    let mock_server = MockServer::start().await;

    let no_closed = mock_server
        .register_as_scoped(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "meta": {
                            "block": {
                                "number": 1,
                                "hash": "hash",
                                "timestamp": 1
                            }
                        },
                        "allocations": []
                    }
                }))),
        )
        .await;

    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        &mock_escrow_subgraph_server.uri(),
        &mock_server.uri(),
        RECEIPT_LIMIT,
    )
    .await;

    // we expect it to create a sender allocation
    sender_account
        .cast(SenderAccountMessage::NewAllocationId(*ALLOCATION_ID_0))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // verify if create sender account
    let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_some());

    // nothing should change because we already created
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(
            vec![*ALLOCATION_ID_0].into_iter().collect(),
        ))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    // try to delete sender allocation_id
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // should not delete it because it was not in network subgraph
    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_some());

    // Mock result for closed allocations

    drop(no_closed);
    mock_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("ClosedAllocations"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {
                        "meta": {
                            "block": {
                                "number": 1,
                                "hash": "hash",
                                "timestamp": 1
                            }
                        },
                        "allocations": [
                            {"id": *ALLOCATION_ID_0 }
                        ]
                    }
                }))),
        )
        .await;

    // try to delete sender allocation_id
    sender_account
        .cast(SenderAccountMessage::UpdateAllocationIds(HashSet::new()))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let actor_ref = ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone());
    assert!(actor_ref.is_none());

    // safely stop the manager
    sender_account.stop_and_wait(None, None).await.unwrap();

    handle.await.unwrap();
}

pub struct MockSenderAllocation {
    triggered_rav_request: Arc<AtomicU32>,
    next_rav_value: Arc<Mutex<u128>>,
    next_unaggregated_fees_value: Arc<Mutex<u128>>,
    receipts: Arc<Mutex<Vec<NewReceiptNotification>>>,

    sender_actor: Option<ActorRef<SenderAccountMessage>>,
}
impl MockSenderAllocation {
    pub fn new_with_triggered_rav_request(
        sender_actor: ActorRef<SenderAccountMessage>,
    ) -> (Self, Arc<AtomicU32>, Arc<Mutex<u128>>) {
        let triggered_rav_request = Arc::new(AtomicU32::new(0));
        let unaggregated_fees = Arc::new(Mutex::new(0));
        (
            Self {
                sender_actor: Some(sender_actor),
                triggered_rav_request: triggered_rav_request.clone(),
                receipts: Arc::new(Mutex::new(Vec::new())),
                next_rav_value: Arc::new(Mutex::new(0)),
                next_unaggregated_fees_value: unaggregated_fees.clone(),
            },
            triggered_rav_request,
            unaggregated_fees,
        )
    }

    pub fn new_with_next_unaggregated_fees_value(
        sender_actor: ActorRef<SenderAccountMessage>,
    ) -> (Self, Arc<Mutex<u128>>) {
        let unaggregated_fees = Arc::new(Mutex::new(0));
        (
            Self {
                sender_actor: Some(sender_actor),
                triggered_rav_request: Arc::new(AtomicU32::new(0)),
                receipts: Arc::new(Mutex::new(Vec::new())),
                next_rav_value: Arc::new(Mutex::new(0)),
                next_unaggregated_fees_value: unaggregated_fees.clone(),
            },
            unaggregated_fees,
        )
    }

    pub fn new_with_next_rav_value(
        sender_actor: ActorRef<SenderAccountMessage>,
    ) -> (Self, Arc<Mutex<u128>>) {
        let next_rav_value = Arc::new(Mutex::new(0));
        (
            Self {
                sender_actor: Some(sender_actor),
                triggered_rav_request: Arc::new(AtomicU32::new(0)),
                receipts: Arc::new(Mutex::new(Vec::new())),
                next_rav_value: next_rav_value.clone(),
                next_unaggregated_fees_value: Arc::new(Mutex::new(0)),
            },
            next_rav_value,
        )
    }

    pub fn new_with_receipts() -> (Self, Arc<Mutex<Vec<NewReceiptNotification>>>) {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sender_actor: None,
                triggered_rav_request: Arc::new(AtomicU32::new(0)),
                receipts: receipts.clone(),
                next_rav_value: Arc::new(Mutex::new(0)),
                next_unaggregated_fees_value: Arc::new(Mutex::new(0)),
            },
            receipts,
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
            SenderAllocationMessage::TriggerRAVRequest => {
                self.triggered_rav_request
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let signed_rav = create_rav(
                    *ALLOCATION_ID_0,
                    SIGNER.0.clone(),
                    4,
                    *self.next_rav_value.lock().unwrap(),
                );
                if let Some(sender_account) = self.sender_actor.as_ref() {
                    sender_account.cast(SenderAccountMessage::UpdateReceiptFees(
                        *ALLOCATION_ID_0,
                        ReceiptFees::RavRequestResponse((
                            UnaggregatedReceipts {
                                value: *self.next_unaggregated_fees_value.lock().unwrap(),
                                last_id: 0,
                                counter: 0,
                            },
                            Ok(Some(signed_rav)),
                        )),
                    ))?;
                }
            }
            SenderAllocationMessage::NewReceipt(receipt) => {
                self.receipts.lock().unwrap().push(receipt);
            }
            _ => {}
        }
        Ok(())
    }
}

async fn create_mock_sender_allocation(
    prefix: String,
    sender: Address,
    allocation: Address,
    sender_actor: ActorRef<SenderAccountMessage>,
) -> (
    Arc<AtomicU32>,
    Arc<Mutex<u128>>,
    ActorRef<SenderAllocationMessage>,
    JoinHandle<()>,
) {
    let (mock_sender_allocation, triggered_rav_request, next_unaggregated_fees) =
        MockSenderAllocation::new_with_triggered_rav_request(sender_actor);

    let name = format!("{}:{}:{}", prefix, sender, allocation);
    let (sender_account, join_handle) =
        MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
            .await
            .unwrap();
    (
        triggered_rav_request,
        next_unaggregated_fees,
        sender_account,
        join_handle,
    )
}

fn get_current_timestamp_u64_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_update_receipt_fees_no_rav(pgpool: PgPool) {
    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let (triggered_rav_request, _, allocation, allocation_handle) =
        create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0, sender_account.clone())
            .await;

    // create a fake sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(TRIGGER_VALUE - 1, get_current_timestamp_u64_ns()),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(BUFFER_MS)).await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_update_receipt_fees_trigger_rav(pgpool: PgPool) {
    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let (triggered_rav_request, _, allocation, allocation_handle) =
        create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0, sender_account.clone())
            .await;

    // create a fake sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    // wait for it to be outside buffer
    tokio::time::sleep(Duration::from_millis(BUFFER_MS)).await;

    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::Retry,
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(BUFFER_MS)).await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_counter_greater_limit_trigger_rav(pgpool: PgPool) {
    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        DUMMY_URL,
        DUMMY_URL,
        2,
    )
    .await;

    let (triggered_rav_request, _, allocation, allocation_handle) =
        create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0, sender_account.clone())
            .await;

    // create a fake sender allocation
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(1, get_current_timestamp_u64_ns()),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(BUFFER_MS)).await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(1, get_current_timestamp_u64_ns()),
        ))
        .unwrap();

    // wait for it to be outside buffer
    tokio::time::sleep(Duration::from_millis(BUFFER_MS)).await;

    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::Retry,
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_remove_sender_account(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        vec![*ALLOCATION_ID_0].into_iter().collect(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        &mock_escrow_subgraph_server.uri(),
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    // check if allocation exists
    let sender_allocation_id = format!("{}:{}:{}", prefix.clone(), SENDER.1, *ALLOCATION_ID_0);
    let Some(sender_allocation) =
        ActorRef::<SenderAllocationMessage>::where_is(sender_allocation_id.clone())
    else {
        panic!("Sender allocation was not created");
    };

    // stop
    sender_account.stop_and_wait(None, None).await.unwrap();

    // check if sender_account is stopped
    assert_eq!(sender_account.get_status(), ActorStatus::Stopped);

    tokio::time::sleep(Duration::from_millis(10)).await;

    // check if sender_allocation is also stopped
    assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

    handle.await.unwrap();
}

/// Test that the deny status is correctly loaded from the DB at the start of the actor
#[sqlx::test(migrations = "../../migrations")]
async fn test_init_deny(pgpool: PgPool) {
    sqlx::query!(
        r#"
                INSERT INTO scalar_tap_denylist (sender_address)
                VALUES ($1)
            "#,
        SENDER.1.encode_hex(),
    )
    .execute(&pgpool)
    .await
    .expect("Should not fail to insert into denylist");

    // make sure there's a reason to keep denied
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    let (sender_account, _handle, _, _) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        TRIGGER_VALUE,
        TRIGGER_VALUE,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(deny);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_retry_unaggregated_fees(pgpool: PgPool) {
    // we set to zero to block the sender, no matter the fee
    let max_unaggregated_fees_per_sender: u128 = 0;

    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        max_unaggregated_fees_per_sender,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let (triggered_rav_request, next_value, allocation, allocation_handle) =
        create_mock_sender_allocation(prefix, SENDER.1, *ALLOCATION_ID_0, sender_account.clone())
            .await;

    assert_eq!(
        triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    *next_value.lock().unwrap() = TRIGGER_VALUE;
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
        ))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let retry_value = triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst);
    assert!(retry_value > 1, "It didn't retry more than once");

    tokio::time::sleep(Duration::from_millis(30)).await;

    let new_value = triggered_rav_request.load(std::sync::atomic::Ordering::SeqCst);
    assert!(new_value > retry_value, "It didn't retry anymore");

    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_deny_allow(pgpool: PgPool) {
    async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
        call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
    }

    let max_unaggregated_fees_per_sender: u128 = 1000;

    // Making sure no RAV is gonna be triggered during the test
    let (sender_account, handle, _, _) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        u128::MAX,
        max_unaggregated_fees_per_sender,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    macro_rules! update_receipt_fees {
        ($value:expr) => {
            sender_account
                .cast(SenderAccountMessage::UpdateReceiptFees(
                    *ALLOCATION_ID_0,
                    ReceiptFees::UpdateValue(UnaggregatedReceipts {
                        value: $value,
                        last_id: 11,
                        counter: 0,
                    }),
                ))
                .unwrap();

            tokio::time::sleep(Duration::from_millis(20)).await;
        };
    }

    macro_rules! update_invalid_receipt_fees {
        ($value:expr) => {
            sender_account
                .cast(SenderAccountMessage::UpdateInvalidReceiptFees(
                    *ALLOCATION_ID_0,
                    UnaggregatedReceipts {
                        value: $value,
                        last_id: 11,
                        counter: 0,
                    },
                ))
                .unwrap();

            tokio::time::sleep(Duration::from_millis(20)).await;
        };
    }

    update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(!deny);

    update_receipt_fees!(max_unaggregated_fees_per_sender);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(!deny);

    update_receipt_fees!(max_unaggregated_fees_per_sender + 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    update_receipt_fees!(max_unaggregated_fees_per_sender - 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(!deny);

    update_receipt_fees!(0);

    update_invalid_receipt_fees!(max_unaggregated_fees_per_sender - 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(!deny);

    update_invalid_receipt_fees!(max_unaggregated_fees_per_sender);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    // invalid receipts should not go down
    update_invalid_receipt_fees!(0);
    let deny = get_deny_status(&sender_account).await;
    // keep denied
    assert!(deny);

    // condition reached using receipts
    update_receipt_fees!(0);
    let deny = get_deny_status(&sender_account).await;
    // allow sender
    assert!(!deny);

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_initialization_with_pending_ravs_over_the_limit(pgpool: PgPool) {
    // add last non-final ravs
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    let (sender_account, handle, _, _) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        TRIGGER_VALUE,
        u128::MAX,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(deny);

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_unaggregated_fees_over_balance(pgpool: PgPool) {
    // add last non-final ravs
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    // other rav final, should not be taken into account
    let signed_rav = create_rav(*ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, true)
        .await
        .unwrap();

    let trigger_rav_request = ESCROW_VALUE * 2;

    // initialize with no trigger value and no max receipt deny
    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        trigger_rav_request,
        u128::MAX,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let (mock_sender_allocation, next_rav_value) =
        MockSenderAllocation::new_with_next_rav_value(sender_account.clone());

    let name = format!("{}:{}:{}", prefix, SENDER.1, *ALLOCATION_ID_0);
    let (allocation, allocation_handle) =
        MockSenderAllocation::spawn(Some(name), mock_sender_allocation, ())
            .await
            .unwrap();

    async fn get_deny_status(sender_account: &ActorRef<SenderAccountMessage>) -> bool {
        call!(sender_account, SenderAccountMessage::GetDeny).unwrap()
    }

    macro_rules! update_receipt_fees {
        ($value:expr) => {
            sender_account
                .cast(SenderAccountMessage::UpdateReceiptFees(
                    *ALLOCATION_ID_0,
                    ReceiptFees::UpdateValue(UnaggregatedReceipts {
                        value: $value,
                        last_id: 11,
                        counter: 0,
                    }),
                ))
                .unwrap();

            tokio::time::sleep(Duration::from_millis(10)).await;
        };
    }

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny);

    let half_escrow = ESCROW_VALUE / 2;
    update_receipt_fees!(half_escrow);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    update_receipt_fees!(half_escrow - 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(!deny);

    update_receipt_fees!(half_escrow + 1);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    update_receipt_fees!(half_escrow + 2);
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);
    // trigger rav request
    // set the unnagregated fees to zero and the rav to the amount
    *next_rav_value.lock().unwrap() = trigger_rav_request;
    update_receipt_fees!(trigger_rav_request);

    // receipt fees should already be 0, but we are setting to 0 again
    update_receipt_fees!(0);

    // should stay denied because the value was transfered to rav
    let deny = get_deny_status(&sender_account).await;
    assert!(deny);

    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_pending_rav_already_redeemed_and_redeem(pgpool: PgPool) {
    // Start a mock graphql server using wiremock
    let mock_server = MockServer::start().await;

    // Mock result for TAP redeem txs for (allocation, sender) pair.
    mock_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("transactions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({ "data": { "transactions": [
                        {"allocationID": *ALLOCATION_ID_0 }
                    ]}}),
                )),
        )
        .await;

    // redeemed
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    let signed_rav = create_rav(*ALLOCATION_ID_1, SIGNER.0.clone(), 4, ESCROW_VALUE - 1);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    let (sender_account, handle, _, escrow_accounts_tx) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        TRIGGER_VALUE,
        u128::MAX,
        &mock_server.uri(),
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny, "should start unblocked");

    mock_server.reset().await;

    // allocation_id sent to the blockchain
    mock_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("transactions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({ "data": { "transactions": [
                        {"allocationID": *ALLOCATION_ID_0 },
                        {"allocationID": *ALLOCATION_ID_1 }
                    ]}}),
                )),
        )
        .await;
    // escrow_account updated
    escrow_accounts_tx
        .send(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .unwrap();

    // wait the actor react to the messages
    tokio::time::sleep(Duration::from_millis(10)).await;

    // should still be active with a 1 escrow available

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny, "should keep unblocked");

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_thawing_deposit_process(pgpool: PgPool) {
    // add last non-final ravs
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, ESCROW_VALUE / 2);
    store_rav_with_options(&pgpool, signed_rav, SENDER.1, true, false)
        .await
        .unwrap();

    let (sender_account, handle, _, escrow_accounts_tx) = create_sender_account(
        pgpool.clone(),
        HashSet::new(),
        TRIGGER_VALUE,
        u128::MAX,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny, "should start unblocked");

    // update the escrow to a lower value
    escrow_accounts_tx
        .send(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE / 2))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(deny, "should block the sender");

    // simulate deposit
    escrow_accounts_tx
        .send(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(ESCROW_VALUE))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny, "should unblock the sender");

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_sender_denied_close_allocation_stop_retry(pgpool: PgPool) {
    // we set to 1 to block the sender on a really low value
    let max_unaggregated_fees_per_sender: u128 = 1;

    let (sender_account, handle, prefix, _) = create_sender_account(
        pgpool,
        HashSet::new(),
        TRIGGER_VALUE,
        max_unaggregated_fees_per_sender,
        DUMMY_URL,
        DUMMY_URL,
        RECEIPT_LIMIT,
    )
    .await;

    let (mock_sender_allocation, next_unaggregated_fees) =
        MockSenderAllocation::new_with_next_unaggregated_fees_value(sender_account.clone());

    let name = format!("{}:{}:{}", prefix, SENDER.1, *ALLOCATION_ID_0);
    let (allocation, allocation_handle) = MockSenderAllocation::spawn_linked(
        Some(name),
        mock_sender_allocation,
        (),
        sender_account.get_cell(),
    )
    .await
    .unwrap();
    *next_unaggregated_fees.lock().unwrap() = TRIGGER_VALUE;

    // set retry
    sender_account
        .cast(SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::NewReceipt(TRIGGER_VALUE, get_current_timestamp_u64_ns()),
        ))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(deny, "should be blocked");

    let scheduler_enabled =
        call!(sender_account, SenderAccountMessage::IsSchedulerEnabled).unwrap();
    assert!(scheduler_enabled, "should have an scheduler enabled");

    // close the allocation and trigger
    allocation.stop_and_wait(None, None).await.unwrap();
    allocation_handle.await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // should remove the block and the retry
    let deny = call!(sender_account, SenderAccountMessage::GetDeny).unwrap();
    assert!(!deny, "should be unblocked");

    let scheuduler_enabled =
        call!(sender_account, SenderAccountMessage::IsSchedulerEnabled).unwrap();
    assert!(!scheuduler_enabled, "should have an scheduler disabled");

    sender_account.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}
