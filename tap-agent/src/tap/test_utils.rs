// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use alloy::{
    primitives::hex::ToHexExt,
    signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    sol_types::eip712_domain,
};
use bigdecimal::num_bigint::BigInt;

use sqlx::types::BigDecimal;

use alloy::dyn_abi::Eip712Domain;
use lazy_static::lazy_static;
use sqlx::PgPool;
use tap_core::{
    rav::{ReceiptAggregateVoucher, SignedRAV},
    receipt::{state::Checking, Receipt, ReceiptWithState, SignedReceipt},
    signed_message::EIP712SignedMessage,
};
use thegraph_core::Address;

lazy_static! {
    pub static ref ALLOCATION_ID_0: Address =
        Address::from_str("0xabababababababababababababababababababab").unwrap();
    pub static ref ALLOCATION_ID_1: Address =
        Address::from_str("0xbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc").unwrap();
    pub static ref SENDER: (PrivateKeySigner, Address) = wallet(0);
    pub static ref SENDER_2: (PrivateKeySigner, Address) = wallet(1);
    pub static ref SIGNER: (PrivateKeySigner, Address) = wallet(2);
    pub static ref INDEXER: (PrivateKeySigner, Address) = wallet(3);
    pub static ref TAP_EIP712_DOMAIN_SEPARATOR: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address:: from([0x11u8; 20]),
    };
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
