// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use alloy_primitives::Address;
use alloy_sol_types::{eip712_domain, Eip712Domain};
use anyhow::Result;
use ethers_signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use lazy_static::lazy_static;
use serde_json;
use sqlx::{types::BigDecimal, PgPool};
use tap_core::receipt_aggregate_voucher::ReceiptAggregateVoucher;
use tap_core::tap_manager::{SignedRAV, SignedReceipt};
use tap_core::tap_receipt::{get_full_list_of_checks, ReceivedReceipt};
use tap_core::{eip_712_signed_message::EIP712SignedMessage, tap_receipt::Receipt};

lazy_static! {
    pub static ref ALLOCATION_ID: Address =
        Address::from_str("0xabababababababababababababababababababab").unwrap();
    pub static ref ALLOCATION_ID_IRRELEVANT: Address =
        Address::from_str("0xbcdebcdebcdebcdebcdebcdebcdebcdebcdebcde").unwrap();
    pub static ref SENDER: (LocalWallet, Address) = wallet(0);
    pub static ref SENDER_IRRELEVANT: (LocalWallet, Address) = wallet(1);
    pub static ref INDEXER: (LocalWallet, Address) = wallet(2);
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
    sender_wallet: &LocalWallet,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
    query_id: u64,
) -> ReceivedReceipt {
    let receipt = EIP712SignedMessage::new(
        &TAP_EIP712_DOMAIN_SEPARATOR,
        Receipt {
            allocation_id: *allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        sender_wallet,
    )
    .await
    .unwrap();
    ReceivedReceipt::new(receipt, query_id, &get_full_list_of_checks())
}

/// Fixture to generate a RAV using the wallet from `keys()`
pub async fn create_rav(
    allocation_id: Address,
    sender_wallet: LocalWallet,
    timestamp_ns: u64,
    value_aggregate: u128,
) -> SignedRAV {
    EIP712SignedMessage::new(
        &TAP_EIP712_DOMAIN_SEPARATOR,
        ReceiptAggregateVoucher {
            allocation_id,
            timestamp_ns,
            value_aggregate,
        },
        &sender_wallet,
    )
    .await
    .unwrap()
}

pub async fn store_receipt(pgpool: &PgPool, signed_receipt: SignedReceipt) -> Result<u64> {
    let record = sqlx::query!(
        r#"
            INSERT INTO scalar_tap_receipts (
                allocation_id, sender_address, timestamp_ns, value, receipt
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id
        "#,
        signed_receipt
            .message
            .allocation_id
            .to_string()
            .trim_start_matches("0x")
            .to_owned(),
        signed_receipt
            .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
            .unwrap()
            .to_string()
            .trim_start_matches("0x")
            .to_owned(),
        BigDecimal::from(signed_receipt.message.timestamp_ns),
        BigDecimal::from_str(&signed_receipt.message.value.to_string())?,
        serde_json::to_value(signed_receipt)?
    )
    .fetch_one(pgpool)
    .await?;

    // id is BIGSERIAL, so it should be safe to cast to u64.
    let id: u64 = record.id.try_into()?;
    Ok(id)
}

pub async fn store_rav(pgpool: &PgPool, signed_rav: SignedRAV, sender: Address) -> Result<()> {
    sqlx::query!(
        r#"
            INSERT INTO scalar_tap_ravs (
                allocation_id, sender_address, rav
            )
            VALUES ($1, $2, $3)
        "#,
        signed_rav
            .message
            .allocation_id
            .to_string()
            .trim_start_matches("0x")
            .to_owned(),
        sender.to_string().trim_start_matches("0x").to_owned(),
        serde_json::to_value(signed_rav).unwrap(),
    )
    .execute(pgpool)
    .await?;

    Ok(())
}
