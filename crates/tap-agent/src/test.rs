// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::num_bigint::BigInt;
use lazy_static::lazy_static;
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{
    rav::{ReceiptAggregateVoucher, SignedRAV},
    receipt::{state::Checking, Receipt, ReceiptWithState, SignedReceipt},
    signed_message::EIP712SignedMessage,
    tap_eip712_domain,
};
use thegraph_core::alloy::{
    primitives::{address, hex::ToHexExt, Address},
    signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    sol_types::Eip712Domain,
};

pub const ALLOCATION_ID_0: Address = address!("abababababababababababababababababababab");
pub const ALLOCATION_ID_1: Address = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");

lazy_static! {
    // pub static ref SENDER: (PrivateKeySigner, Address) = wallet(0);
    pub static ref SENDER_2: (PrivateKeySigner, Address) = wallet(1);
    pub static ref SIGNER: (PrivateKeySigner, Address) = wallet(2);
    pub static ref INDEXER: (PrivateKeySigner, Address) = wallet(3);
    pub static ref TAP_EIP712_DOMAIN_SEPARATOR: Eip712Domain =
        tap_eip712_domain(1, Address::from([0x11u8; 20]),);
}

/// Fixture to generate a RAV using the wallet from `keys()`
pub fn create_rav(
    allocation_id: Address,
    signer_wallet: PrivateKeySigner,
    timestamp_ns: u64,
    value_aggregate: u128,
) -> SignedRAV {
    EIP712SignedMessage::new(
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

/// Fixture to generate a signed receipt using the wallet from `keys()` and the
/// given `query_id` and `value`
pub fn create_received_receipt(
    allocation_id: &Address,
    signer_wallet: &PrivateKeySigner,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
) -> ReceiptWithState<Checking> {
    let receipt = EIP712SignedMessage::new(
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
    ReceiptWithState::new(receipt)
}

pub async fn store_receipt(pgpool: &PgPool, signed_receipt: &SignedReceipt) -> anyhow::Result<u64> {
    let encoded_signature = signed_receipt.signature.as_bytes().to_vec();

    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
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

pub async fn store_invalid_receipt(
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
    signed_rav: SignedRAV,
    sender: Address,
) -> anyhow::Result<()> {
    store_rav_with_options(pgpool, signed_rav, sender, false, false).await
}

pub async fn store_rav_with_options(
    pgpool: &PgPool,
    signed_rav: SignedRAV,
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
    use std::sync::Arc;

    use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
    use test_assets::{ALLOCATION_ID_0, TAP_SIGNER};
    use thegraph_core::alloy::primitives::Address;
    use tokio::sync::{mpsc, watch, Notify};

    use super::create_rav;
    use crate::agent::{
        sender_account::{ReceiptFees, SenderAccountMessage},
        sender_accounts_manager::NewReceiptNotification,
        sender_allocation::SenderAllocationMessage,
        unaggregated_receipts::UnaggregatedReceipts,
    };

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
        pub notify: Arc<Notify>,
    }

    impl<T> TestableActor<T>
    where
        T: Actor,
    {
        pub fn new(inner: T) -> Self {
            Self {
                inner,
                notify: Arc::new(Notify::new()),
            }
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
            let result = self.inner.handle(myself, msg, state).await;
            self.notify.notify_one();
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

    pub struct MockSenderAllocation {
        triggered_rav_request: Arc<Notify>,
        sender_actor: Option<ActorRef<SenderAccountMessage>>,

        next_rav_value: watch::Receiver<u128>,
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
                    next_rav_value: watch::channel(0).1,
                    next_unaggregated_fees_value,
                },
                triggered_rav_request,
                unaggregated_fees,
            )
        }

        pub fn new_with_next_rav_value(
            sender_actor: ActorRef<SenderAccountMessage>,
        ) -> (Self, watch::Sender<u128>) {
            let (next_rav_value_sender, next_rav_value) = watch::channel(0);
            (
                Self {
                    sender_actor: Some(sender_actor),
                    triggered_rav_request: Arc::new(Notify::new()),
                    receipts: mpsc::channel(1).0,
                    next_rav_value,
                    next_unaggregated_fees_value: watch::channel(0).1,
                },
                next_rav_value_sender,
            )
        }

        pub fn new_with_receipts() -> (Self, mpsc::Receiver<NewReceiptNotification>) {
            let (tx, rx) = mpsc::channel(10);

            (
                Self {
                    sender_actor: None,
                    triggered_rav_request: Arc::new(Notify::new()),
                    receipts: tx,
                    next_rav_value: watch::channel(0).1,
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
                SenderAllocationMessage::TriggerRAVRequest => {
                    self.triggered_rav_request.notify_one();
                    if let Some(sender_account) = self.sender_actor.as_ref() {
                        let signed_rav = create_rav(
                            ALLOCATION_ID_0,
                            TAP_SIGNER.0.clone(),
                            4,
                            *self.next_rav_value.borrow(),
                        );
                        sender_account.cast(SenderAccountMessage::UpdateReceiptFees(
                            ALLOCATION_ID_0,
                            ReceiptFees::RavRequestResponse(
                                UnaggregatedReceipts {
                                    value: *self.next_unaggregated_fees_value.borrow(),
                                    last_id: 0,
                                    counter: 0,
                                },
                                Ok(Some(signed_rav)),
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

        let name = format!("{}:{}:{}", prefix, sender, allocation);
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
