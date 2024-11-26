
use super::{
    SenderAllocation, SenderAllocationArgs, SenderAllocationMessage, SenderAllocationState,
};
use crate::{
    agent::{
        sender_account::{ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::NewReceiptNotification,
        unaggregated_receipts::UnaggregatedReceipts,
    },
    tap::test_utils::{
        create_rav, create_received_receipt, store_invalid_receipt, store_rav, store_receipt,
        ALLOCATION_ID_0, INDEXER, SENDER, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    },
};
use futures::future::join_all;
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use jsonrpsee::http_client::HttpClientBuilder;
use ractor::{
    call, cast, concurrency::JoinHandle, Actor, ActorProcessingErr, ActorRef, ActorStatus,
};
use ruint::aliases::U256;
use serde_json::json;
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tap_aggregator::{jsonrpsee_helpers::JsonRpcResponse, server::run_server};
use tap_core::receipt::{
    checks::{Check, CheckError, CheckList, CheckResult},
    state::Checking,
    Context, ReceiptWithState,
};
use tokio::sync::{mpsc, watch};
use wiremock::{
    matchers::{body_string_contains, method},
    Mock, MockGuard, MockServer, Respond, ResponseTemplate,
};

const DUMMY_URL: &str = "http://localhost:1234";

pub struct MockSenderAccount {
    pub last_message_emitted: tokio::sync::mpsc::Sender<SenderAccountMessage>,
}

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

#[async_trait::async_trait]
impl Actor for MockSenderAccount {
    type Msg = SenderAccountMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _allocation_ids: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        self.last_message_emitted.send(message).await.unwrap();
        Ok(())
    }
}

async fn create_mock_sender_account() -> (
    mpsc::Receiver<SenderAccountMessage>,
    ActorRef<SenderAccountMessage>,
    JoinHandle<()>,
) {
    let (last_message_emitted, rx) = mpsc::channel(64);

    let (sender_account, join_handle) = MockSenderAccount::spawn(
        None,
        MockSenderAccount {
            last_message_emitted: last_message_emitted.clone(),
        },
        (),
    )
    .await
    .unwrap();
    (rx, sender_account, join_handle)
}

async fn create_sender_allocation_args(
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
    escrow_subgraph_endpoint: &str,
    sender_account: Option<ActorRef<SenderAccountMessage>>,
) -> SenderAllocationArgs {
    let escrow_subgraph = Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(escrow_subgraph_endpoint).unwrap(),
        )
        .await,
    ));

    let escrow_accounts_rx = watch::channel(EscrowAccounts::new(
        HashMap::from([(SENDER.1, U256::from(1000))]),
        HashMap::from([(SENDER.1, vec![SIGNER.1])]),
    ))
    .1;

    let sender_account_ref = match sender_account {
        Some(sender) => sender,
        None => create_mock_sender_account().await.1,
    };

    let sender_aggregator = HttpClientBuilder::default()
        .build(&sender_aggregator_endpoint)
        .unwrap();
    SenderAllocationArgs {
        pgpool: pgpool.clone(),
        allocation_id: *ALLOCATION_ID_0,
        sender: SENDER.1,
        escrow_accounts: escrow_accounts_rx,
        escrow_subgraph,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        sender_account_ref,
        sender_aggregator,
        config: super::AllocationConfig {
            timestamp_buffer_ns: 1,
            rav_request_receipt_limit: 1000,
            indexer_address: INDEXER.1,
            escrow_polling_interval: Duration::from_millis(1000),
        },
    }
}

async fn create_sender_allocation(
    pgpool: PgPool,
    sender_aggregator_endpoint: String,
    escrow_subgraph_endpoint: &str,
    sender_account: Option<ActorRef<SenderAccountMessage>>,
) -> ActorRef<SenderAllocationMessage> {
    let args = create_sender_allocation_args(
        pgpool,
        sender_aggregator_endpoint,
        escrow_subgraph_endpoint,
        sender_account,
    )
    .await;

    let (allocation_ref, _join_handle) = SenderAllocation::spawn(None, SenderAllocation, args)
        .await
        .unwrap();

    allocation_ref
}

#[sqlx::test(migrations = "../../migrations")]
async fn should_update_unaggregated_fees_on_start(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let (mut last_message_emitted, sender_account, _join_handle) =
        create_mock_sender_account().await;
    // Add receipts to the database.
    for i in 1..=10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        Some(sender_account),
    )
    .await;

    // Get total_unaggregated_fees
    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::GetUnaggregatedReceipts
    )
    .unwrap();

    // Should emit a message to the sender account with the unaggregated fees.
    let expected_message = SenderAccountMessage::UpdateReceiptFees(
        *ALLOCATION_ID_0,
        ReceiptFees::UpdateValue(UnaggregatedReceipts {
            last_id: 10,
            value: 55u128,
            counter: 10,
        }),
    );
    let last_message_emitted = last_message_emitted.recv().await.unwrap();
    assert_eq!(last_message_emitted, expected_message);

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 55u128);
}

#[sqlx::test(migrations = "../../migrations")]
async fn should_return_invalid_receipts_on_startup(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;
    // Add receipts to the database.
    for i in 1..=10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_invalid_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        Some(sender_account),
    )
    .await;

    // Get total_unaggregated_fees
    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::GetUnaggregatedReceipts
    )
    .unwrap();

    // Should emit a message to the sender account with the unaggregated fees.
    let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
        *ALLOCATION_ID_0,
        UnaggregatedReceipts {
            last_id: 10,
            value: 55u128,
            counter: 10,
        },
    );
    let update_invalid_msg = message_receiver.recv().await.unwrap();
    assert_eq!(update_invalid_msg, expected_message);
    let last_message_emitted = message_receiver.recv().await.unwrap();
    assert_eq!(
        last_message_emitted,
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
        )
    );

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 0u128);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_receive_new_receipt(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;

    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        Some(sender_account),
    )
    .await;

    // should validate with id less than last_id
    cast!(
        sender_allocation,
        SenderAllocationMessage::NewReceipt(NewReceiptNotification {
            id: 0,
            value: 10,
            allocation_id: *ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            timestamp_ns: 0,
        })
    )
    .unwrap();

    let timestamp_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    cast!(
        sender_allocation,
        SenderAllocationMessage::NewReceipt(NewReceiptNotification {
            id: 1,
            value: 20,
            allocation_id: *ALLOCATION_ID_0,
            signer_address: SIGNER.1,
            timestamp_ns,
        })
    )
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // should emit update aggregate fees message to sender account
    let expected_message = SenderAccountMessage::UpdateReceiptFees(
        *ALLOCATION_ID_0,
        ReceiptFees::NewReceipt(20u128, timestamp_ns),
    );
    let startup_load_msg = message_receiver.recv().await.unwrap();
    assert_eq!(
        startup_load_msg,
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                value: 0,
                last_id: 0,
                counter: 0,
            })
        )
    );
    let last_message_emitted = message_receiver.recv().await.unwrap();
    assert_eq!(last_message_emitted, expected_message);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_trigger_rav_request(pgpool: PgPool) {
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

    // Add receipts to the database.
    for i in 0..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();

        // store a copy that should fail in the uniqueness test
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;

    // Create a sender_allocation.
    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        "http://".to_owned() + &aggregator_endpoint.to_string(),
        &mock_server.uri(),
        Some(sender_account),
    )
    .await;

    // Trigger a RAV request manually and wait for updated fees.
    sender_allocation
        .cast(SenderAllocationMessage::TriggerRAVRequest)
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let total_unaggregated_fees = call!(
        sender_allocation,
        SenderAllocationMessage::GetUnaggregatedReceipts
    )
    .unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 0u128);

    let startup_msg = message_receiver.recv().await.unwrap();
    assert_eq!(
        startup_msg,
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                value: 90,
                last_id: 20,
                counter: 20,
            })
        )
    );

    // Check if the sender received invalid receipt fees
    let expected_message = SenderAccountMessage::UpdateInvalidReceiptFees(
        *ALLOCATION_ID_0,
        UnaggregatedReceipts {
            last_id: 0,
            value: 45u128,
            counter: 0,
        },
    );
    assert_eq!(message_receiver.recv().await.unwrap(), expected_message);

    assert!(matches!(
        message_receiver.recv().await.unwrap(),
        SenderAccountMessage::UpdateReceiptFees(_, ReceiptFees::RavRequestResponse(_))
    ));

    // Stop the TAP aggregator server.
    handle.stop().unwrap();
    handle.stopped().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_close_allocation_no_pending_fees(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;

    // create allocation
    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        Some(sender_account),
    )
    .await;

    sender_allocation.stop_and_wait(None, None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // check if the actor is actually stopped
    assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);

    // check if message is sent to sender account
    assert_eq!(
        message_receiver.recv().await.unwrap(),
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts::default())
        )
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_close_allocation_with_pending_fees(pgpool: PgPool) {
    struct Response {
        data: Arc<tokio::sync::Notify>,
    }

    impl Respond for Response {
        fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
            self.data.notify_one();

            let mock_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 10, 45);

            let json_response = JsonRpcResponse {
                data: mock_rav,
                warnings: None,
            };

            ResponseTemplate::new(200).set_body_json(json! (
                {
                    "id": 0,
                    "jsonrpc": "2.0",
                    "result": json_response
                }
            ))
        }
    }

    let await_trigger = Arc::new(tokio::sync::Notify::new());
    // Start a TAP aggregator server.
    let aggregator_server = MockServer::start().await;

    aggregator_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("aggregate_receipts"))
                .respond_with(Response {
                    data: await_trigger.clone(),
                }),
        )
        .await;

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

    // Add receipts to the database.
    for i in 0..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i + 1, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let (_last_message_emitted, sender_account, _join_handle) = create_mock_sender_account().await;

    // create allocation
    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        aggregator_server.uri(),
        &mock_server.uri(),
        Some(sender_account),
    )
    .await;

    sender_allocation.stop_and_wait(None, None).await.unwrap();

    // should trigger rav request
    await_trigger.notified().await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // check if rav request is made
    assert!(aggregator_server.received_requests().await.is_some());

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // check if the actor is actually stopped
    assert_eq!(sender_allocation.get_status(), ActorStatus::Stopped);
}

#[sqlx::test(migrations = "../../migrations")]
async fn should_return_unaggregated_fees_without_rav(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let state = SenderAllocationState::new(args).await.unwrap();

    // Add receipts to the database.
    for i in 1..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    // calculate unaggregated fee
    let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 45u128);
}

#[sqlx::test(migrations = "../../migrations")]
async fn should_calculate_invalid_receipts_fee(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let state = SenderAllocationState::new(args).await.unwrap();

    // Add receipts to the database.
    for i in 1..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_invalid_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    // calculate invalid unaggregated fee
    let total_invalid_receipts = state.calculate_invalid_receipts_fee().await.unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_invalid_receipts.value, 45u128);
}

/// Test that the sender_allocation correctly updates the unaggregated fees from the
/// database when there is a RAV in the database as well as receipts which timestamp are lesser
/// and greater than the RAV's timestamp.
///
/// The sender_allocation should only consider receipts with a timestamp greater
/// than the RAV's timestamp.
#[sqlx::test(migrations = "../../migrations")]
async fn should_return_unaggregated_fees_with_rav(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let state = SenderAllocationState::new(args).await.unwrap();

    // Add the RAV to the database.
    // This RAV has timestamp 4. The sender_allocation should only consider receipts
    // with a timestamp greater than 4.
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
    store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

    // Add receipts to the database.
    for i in 1..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let total_unaggregated_fees = state.recalculate_all_unaggregated_fees().await.unwrap();

    // Check that the unaggregated fees are correct.
    assert_eq!(total_unaggregated_fees.value, 35u128);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_store_failed_rav(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let state = SenderAllocationState::new(args).await.unwrap();

    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);

    // just unit test if it is working
    let result = state
        .store_failed_rav(&signed_rav.message, &signed_rav, "test")
        .await;

    assert!(result.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_store_invalid_receipts(pgpool: PgPool) {
    struct FailingCheck;

    #[async_trait::async_trait]
    impl Check for FailingCheck {
        async fn check(
            &self,
            _: &tap_core::receipt::Context,
            _receipt: &ReceiptWithState<Checking>,
        ) -> CheckResult {
            Err(CheckError::Failed(anyhow::anyhow!("Failing check")))
        }
    }

    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let mut state = SenderAllocationState::new(args).await.unwrap();

    let checks = CheckList::new(vec![Arc::new(FailingCheck)]);

    // create some checks
    let checking_receipts = vec![
        create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1u128),
        create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, 2, 2, 2u128),
    ];
    // make sure to fail them
    let failing_receipts = checking_receipts
        .into_iter()
        .map(|receipt| async {
            receipt
                .finalize_receipt_checks(&Context::new(), &checks)
                .await
                .unwrap()
                .unwrap_err()
        })
        .collect::<Vec<_>>();
    let failing_receipts: Vec<_> = join_all(failing_receipts).await;

    // store the failing receipts
    let result = state.store_invalid_receipts(&failing_receipts).await;

    // we just store a few and make sure it doesn't fail
    assert!(result.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_mark_rav_last(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;
    let signed_rav = create_rav(*ALLOCATION_ID_0, SIGNER.0.clone(), 4, 10);
    store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

    let args = create_sender_allocation_args(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        None,
    )
    .await;
    let state = SenderAllocationState::new(args).await.unwrap();

    // mark rav as final
    let result = state.mark_rav_last().await;

    // check if it fails
    assert!(result.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_failed_rav_request(pgpool: PgPool) {
    let (mock_escrow_subgraph_server, _mock_ecrow_subgraph) = mock_escrow_subgraph().await;

    // Add receipts to the database.
    for i in 0..10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, u64::MAX, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;

    // Create a sender_allocation.
    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        DUMMY_URL.to_string(),
        &mock_escrow_subgraph_server.uri(),
        Some(sender_account),
    )
    .await;

    // Trigger a RAV request manually and wait for updated fees.
    // this should fail because there's no receipt with valid timestamp
    sender_allocation
        .cast(SenderAllocationMessage::TriggerRAVRequest)
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // If it is an error then rav request failed

    let startup_msg = message_receiver.recv().await.unwrap();
    assert_eq!(
        startup_msg,
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                value: 45,
                last_id: 10,
                counter: 10,
            })
        )
    );
    let rav_response_message = message_receiver.recv().await.unwrap();
    match rav_response_message {
        SenderAccountMessage::UpdateReceiptFees(
            _,
            ReceiptFees::RavRequestResponse(rav_response),
        ) => {
            assert!(rav_response.1.is_err());
        }
        v => panic!("Expecting RavRequestResponse as last message, found: {v:?}"),
    }

    // expect the actor to keep running
    assert_eq!(sender_allocation.get_status(), ActorStatus::Running);

    // Check that the unaggregated fees return the same value
    // TODO: Maybe this can no longer be checked?
    //assert_eq!(total_unaggregated_fees.value, 45u128);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_rav_request_when_all_receipts_invalid(pgpool: PgPool) {
    // Start a TAP aggregator server.
    let (_handle, aggregator_endpoint) = run_server(
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

    // Start a mock graphql server using wiremock
    let mock_server = MockServer::start().await;

    // Mock result for TAP redeem txs for (allocation, sender) pair.
    mock_server
        .register(
            Mock::given(method("POST"))
                .and(body_string_contains("transactions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({ "data": { "transactions": [
                        {
                            "id": "redeemed"
                        }
                    ]}}),
                )),
        )
        .await;
    // Add invalid receipts to the database. ( already redeemed )
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos() as u64;
    const RECEIPT_VALUE: u128 = 1622018441284756158;
    const TOTAL_RECEIPTS: u64 = 10;
    const TOTAL_SUM: u128 = RECEIPT_VALUE * TOTAL_RECEIPTS as u128;

    for i in 0..TOTAL_RECEIPTS {
        let receipt =
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, timestamp, RECEIPT_VALUE);
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    let (mut message_receiver, sender_account, _join_handle) = create_mock_sender_account().await;

    let sender_allocation = create_sender_allocation(
        pgpool.clone(),
        "http://".to_owned() + &aggregator_endpoint.to_string(),
        &mock_server.uri(),
        Some(sender_account),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    // Trigger a RAV request manually and wait for updated fees.
    // this should fail because there's no receipt with valid timestamp
    sender_allocation
        .cast(SenderAllocationMessage::TriggerRAVRequest)
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // If it is an error then rav request failed

    let startup_msg = message_receiver.recv().await.unwrap();
    assert_eq!(
        startup_msg,
        SenderAccountMessage::UpdateReceiptFees(
            *ALLOCATION_ID_0,
            ReceiptFees::UpdateValue(UnaggregatedReceipts {
                value: 16220184412847561580,
                last_id: 10,
                counter: 10,
            })
        )
    );

    let invalid_receipts = message_receiver.recv().await.unwrap();

    assert_eq!(
        invalid_receipts,
        SenderAccountMessage::UpdateInvalidReceiptFees(
            *ALLOCATION_ID_0,
            UnaggregatedReceipts {
                value: TOTAL_SUM,
                last_id: 0,
                counter: 0,
            }
        )
    );

    let rav_response_message = message_receiver.recv().await.unwrap();
    match rav_response_message {
        SenderAccountMessage::UpdateReceiptFees(
            _,
            ReceiptFees::RavRequestResponse(rav_response),
        ) => {
            assert!(rav_response.1.is_err());
        }
        v => panic!("Expecting RavRequestResponse as last message, found: {v:?}"),
    }

    let invalid_receipts = sqlx::query!(
        r#"
                SELECT * FROM scalar_tap_receipts_invalid;
            "#,
    )
    .fetch_all(&pgpool)
    .await
    .expect("Should not fail to fetch from scalar_tap_receipts_invalid");

    // Invalid receipts should be found inside the table
    assert_eq!(invalid_receipts.len(), 10);

    // make sure scalar_tap_receipts gets emptied
    let all_receipts = sqlx::query!(
        r#"
                SELECT * FROM scalar_tap_receipts;
            "#,
    )
    .fetch_all(&pgpool)
    .await
    .expect("Should not fail to fetch from scalar_tap_receipts");

    // Invalid receipts should be found inside the table
    assert!(all_receipts.is_empty());
}
