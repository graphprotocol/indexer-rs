use super::{
    new_receipts_watcher, SenderAccountsManager, SenderAccountsManagerArgs,
    SenderAccountsManagerMessage, State,
};
use crate::agent::sender_account::tests::{MockSenderAllocation, PREFIX_ID};
use crate::agent::sender_account::{SenderAccountConfig, SenderAccountMessage};
use crate::agent::sender_accounts_manager::{
    receipt_watcher::handle_notification, NewReceiptNotification,
};
use crate::agent::sender_allocation::tests::MockSenderAccount;
use crate::tap::test_utils::{
    create_rav, create_received_receipt, store_rav, store_receipt, ALLOCATION_ID_0,
    ALLOCATION_ID_1, INDEXER, SENDER, SENDER_2, SENDER_3, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
};
use alloy::hex::ToHexExt;
use indexer_monitor::{DeploymentDetails, EscrowAccounts, SubgraphClient};
use ractor::concurrency::JoinHandle;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::Url;
use ruint::aliases::U256;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const DUMMY_URL: &str = "http://localhost:1234";

async fn get_subgraph_client() -> &'static SubgraphClient {
    Box::leak(Box::new(
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(DUMMY_URL).unwrap(),
        )
        .await,
    ))
}

fn get_config() -> &'static super::SenderAccountConfig {
    Box::leak(Box::new(SenderAccountConfig {
        rav_request_buffer: Duration::from_millis(1),
        max_amount_willing_to_lose_grt: 0,
        trigger_value: 100,
        rav_request_timeout: Duration::from_millis(1),
        rav_request_receipt_limit: 1000,
        indexer_address: INDEXER.1,
        escrow_polling_interval: Duration::default(),
    }))
}

async fn create_sender_accounts_manager(
    pgpool: PgPool,
) -> (
    String,
    (ActorRef<SenderAccountsManagerMessage>, JoinHandle<()>),
) {
    let config = get_config();

    let (_allocations_tx, allocations_rx) = watch::channel(HashMap::new());
    let escrow_subgraph = get_subgraph_client().await;
    let network_subgraph = get_subgraph_client().await;

    let (_, escrow_accounts_rx) = watch::channel(EscrowAccounts::default());

    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let args = SenderAccountsManagerArgs {
        config,
        domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
        pgpool,
        indexer_allocations: allocations_rx,
        escrow_accounts: escrow_accounts_rx,
        escrow_subgraph,
        network_subgraph,
        sender_aggregator_endpoints: HashMap::from([
            (SENDER.1, Url::parse("http://localhost:8000").unwrap()),
            (SENDER_2.1, Url::parse("http://localhost:8000").unwrap()),
        ]),
        prefix: Some(prefix.clone()),
    };
    (
        prefix,
        SenderAccountsManager::spawn(None, SenderAccountsManager, args)
            .await
            .unwrap(),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_create_sender_accounts_manager(pgpool: PgPool) {
    let (_, (actor, join_handle)) = create_sender_accounts_manager(pgpool).await;
    actor.stop_and_wait(None, None).await.unwrap();
    join_handle.await.unwrap();
}

async fn create_state(pgpool: PgPool) -> (String, State) {
    let config = get_config();
    let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
    let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);

    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    (
        prefix.clone(),
        State {
            config,
            domain_separator: TAP_EIP712_DOMAIN_SEPARATOR.clone(),
            sender_ids: HashSet::new(),
            new_receipts_watcher_handle: None,
            _eligible_allocations_senders_handle: tokio::spawn(async move {}),
            pgpool,
            indexer_allocations: watch::channel(HashSet::new()).1,
            escrow_accounts: watch::channel(escrow_accounts).1,
            escrow_subgraph: get_subgraph_client().await,
            network_subgraph: get_subgraph_client().await,
            sender_aggregator_endpoints: HashMap::from([
                (SENDER.1, Url::parse("http://localhost:8000").unwrap()),
                (SENDER_2.1, Url::parse("http://localhost:8000").unwrap()),
            ]),
            prefix: Some(prefix),
        },
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_pending_sender_allocations(pgpool: PgPool) {
    let (_, state) = create_state(pgpool.clone()).await;

    // add receipts to the database
    for i in 1..=10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    // add non-final ravs
    let signed_rav = create_rav(*ALLOCATION_ID_1, SIGNER.0.clone(), 4, 10);
    store_rav(&pgpool, signed_rav, SENDER.1).await.unwrap();

    let pending_allocation_id = state.get_pending_sender_allocation_id().await;

    // check if pending allocations are correct
    assert_eq!(pending_allocation_id.len(), 1);
    assert!(pending_allocation_id.contains_key(&SENDER.1));
    assert_eq!(pending_allocation_id.get(&SENDER.1).unwrap().len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_update_sender_allocation(pgpool: PgPool) {
    let (prefix, (actor, join_handle)) = create_sender_accounts_manager(pgpool).await;

    actor
        .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
            vec![SENDER.1].into_iter().collect(),
        ))
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // verify if create sender account
    let actor_ref =
        ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix.clone(), SENDER.1));
    assert!(actor_ref.is_some());

    actor
        .cast(SenderAccountsManagerMessage::UpdateSenderAccounts(
            HashSet::new(),
        ))
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // verify if it gets removed
    let actor_ref = ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER.1));
    assert!(actor_ref.is_none());

    // safely stop the manager
    actor.stop_and_wait(None, None).await.unwrap();
    join_handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_create_sender_account(pgpool: PgPool) {
    struct DummyActor;
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

    let (prefix, state) = create_state(pgpool.clone()).await;
    let (supervisor, handle) = DummyActor::spawn(None, DummyActor, ()).await.unwrap();
    // we wait to check if the sender is created

    state
        .create_sender_account(supervisor.get_cell(), SENDER_2.1, HashSet::new())
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let actor_ref =
        ActorRef::<SenderAccountMessage>::where_is(format!("{}:{}", prefix, SENDER_2.1));
    assert!(actor_ref.is_some());

    supervisor.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_deny_sender_account_on_failure(pgpool: PgPool) {
    struct DummyActor;
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

    let (_prefix, state) = create_state(pgpool.clone()).await;
    let (supervisor, handle) = DummyActor::spawn(None, DummyActor, ()).await.unwrap();
    // we wait to check if the sender is created

    let sender_id = SENDER_3.1;

    state
        .create_or_deny_sender(supervisor.get_cell(), sender_id, HashSet::new())
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // TODO check if sender is denied

    let denied = sqlx::query!(
        r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM scalar_tap_denylist
                    WHERE sender_address = $1
                ) as denied
            "#,
        sender_id.encode_hex(),
    )
    .fetch_one(&pgpool)
    .await
    .unwrap()
    .denied
    .expect("Deny status cannot be null");

    assert!(denied, "Sender was not denied after failing.");

    supervisor.stop_and_wait(None, None).await.unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_receive_notifications_(pgpool: PgPool) {
    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    // create dummy allocation

    let (mock_sender_allocation, receipts) = MockSenderAllocation::new_with_receipts();
    let _ = MockSenderAllocation::spawn(
        Some(format!(
            "{}:{}:{}",
            prefix.clone(),
            SENDER.1,
            *ALLOCATION_ID_0
        )),
        mock_sender_allocation,
        (),
    )
    .await
    .unwrap();

    // create tokio task to listen for notifications

    let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
    pglistener
        .listen("scalar_tap_receipt_notification")
        .await
        .expect(
            "should be able to subscribe to Postgres Notify events on the channel \
                'scalar_tap_receipt_notification'",
        );

    let escrow_accounts_rx = watch::channel(EscrowAccounts::new(
        HashMap::from([(SENDER.1, U256::from(1000))]),
        HashMap::from([(SENDER.1, vec![SIGNER.1])]),
    ))
    .1;

    // Start the new_receipts_watcher task that will consume from the `pglistener`
    let new_receipts_watcher_handle = tokio::spawn(new_receipts_watcher(
        pglistener,
        escrow_accounts_rx,
        Some(prefix.clone()),
    ));

    // add receipts to the database
    for i in 1..=10 {
        let receipt = create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, i, i, i.into());
        store_receipt(&pgpool, receipt.signed_receipt())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // check if receipt notification was sent to the allocation
    let receipts = receipts.lock().unwrap();
    assert_eq!(receipts.len(), 10);
    for (i, receipt) in receipts.iter().enumerate() {
        assert_eq!((i + 1) as u64, receipt.id);
    }

    new_receipts_watcher_handle.abort();
}

#[tokio::test]
async fn test_create_allocation_id() {
    let senders_to_signers = vec![(SENDER.1, vec![SIGNER.1])].into_iter().collect();
    let escrow_accounts = EscrowAccounts::new(HashMap::new(), senders_to_signers);
    let escrow_accounts = watch::channel(escrow_accounts).1;

    let prefix = format!(
        "test-{}",
        PREFIX_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );

    let (last_message_emitted, mut rx) = mpsc::channel(64);

    let (sender_account, join_handle) = MockSenderAccount::spawn(
        Some(format!("{}:{}", prefix.clone(), SENDER.1,)),
        MockSenderAccount {
            last_message_emitted,
        },
        (),
    )
    .await
    .unwrap();

    let new_receipt_notification = NewReceiptNotification {
        id: 1,
        allocation_id: *ALLOCATION_ID_0,
        signer_address: SIGNER.1,
        timestamp_ns: 1,
        value: 1,
    };

    handle_notification(new_receipt_notification, escrow_accounts, Some(&prefix))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    assert_eq!(
        rx.recv().await.unwrap(),
        SenderAccountMessage::NewAllocationId(*ALLOCATION_ID_0)
    );
    sender_account.stop_and_wait(None, None).await.unwrap();
    join_handle.await.unwrap();
}
