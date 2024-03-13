// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use alloy_primitives::hex::ToHex;
use alloy_sol_types::{eip712_domain, Eip712Domain};
use anyhow::Result;
use bigdecimal::num_bigint::BigInt;
use ethers_signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use lazy_static::lazy_static;
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{
    rav::{ReceiptAggregateVoucher, SignedRAV},
    receipt::{Checking, Receipt, ReceiptWithState, SignedReceipt},
    signed_message::EIP712SignedMessage,
};
use thegraph::types::Address;

lazy_static! {
    pub static ref ALLOCATION_ID_0: Address =
        Address::from_str("0xabababababababababababababababababababab").unwrap();
    pub static ref ALLOCATION_ID_1: Address =
        Address::from_str("0xbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc").unwrap();
    pub static ref ALLOCATION_ID_2: Address =
        Address::from_str("0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd").unwrap();
    pub static ref ALLOCATION_ID_IRRELEVANT: Address =
        Address::from_str("0xbcdebcdebcdebcdebcdebcdebcdebcdebcdebcde").unwrap();
    pub static ref SENDER: (LocalWallet, Address) = wallet(0);
    pub static ref SENDER_IRRELEVANT: (LocalWallet, Address) = wallet(1);
    pub static ref SIGNER: (LocalWallet, Address) = wallet(2);
    pub static ref INDEXER: (LocalWallet, Address) = wallet(3);
    pub static ref TAP_EIP712_DOMAIN_SEPARATOR: Eip712Domain = eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address:: from([0x11u8; 20]),
    };
}

/// Fixture to generate a wallet and address
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

/// Fixture to generate a signed receipt using the wallet from `keys()` and the
/// given `query_id` and `value`
pub async fn create_received_receipt(
    allocation_id: &Address,
    signer_wallet: &LocalWallet,
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

/// Fixture to generate a RAV using the wallet from `keys()`
pub async fn create_rav(
    allocation_id: Address,
    signer_wallet: LocalWallet,
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

pub async fn store_receipt(pgpool: &PgPool, signed_receipt: &SignedReceipt) -> Result<u64> {
    let encoded_signature = signed_receipt.signature.to_vec();

    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#,
        signed_receipt
            .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
            .unwrap()
            .encode_hex::<String>(),
        encoded_signature,
        signed_receipt.message.allocation_id.encode_hex::<String>(),
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

pub async fn store_rav(pgpool: &PgPool, signed_rav: SignedRAV, sender: Address) -> Result<()> {
    let signature_bytes = signed_rav.signature.to_vec();

    let _fut = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_ravs (sender_address, signature, allocation_id, timestamp_ns, value_aggregate)
            VALUES ($1, $2, $3, $4, $5)
        "#,
        sender.encode_hex::<String>(),
        signature_bytes,
        signed_rav.message.allocationId.encode_hex::<String>(),
        BigDecimal::from(signed_rav.message.timestampNs),
        BigDecimal::from(BigInt::from(signed_rav.message.valueAggregate)),
    )
    .execute(pgpool)
    .await?;

    Ok(())
}
