// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::{bytes::BytesMut, hex::ToHex};
use anyhow::anyhow;
use ethers::types::Signature;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use open_fastrlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use thegraph::types::Address;

mod escrow_adapter;
mod rav_storage_adapter;
mod receipt_checks_adapter;
mod receipt_storage_adapter;
mod sender_account;
pub mod sender_accounts_manager;
mod sender_allocation;
mod unaggregated_receipts;

#[cfg(test)]
pub mod test_utils;

async fn signers_trimmed(
    escrow_accounts: &Eventual<EscrowAccounts>,
    sender: Address,
) -> Result<Vec<String>, anyhow::Error> {
    let signers = escrow_accounts
        .value()
        .await
        .map_err(|e| anyhow!("Error while getting escrow accounts: {:?}", e))?
        .get_signers_for_sender(&sender)?
        .iter()
        .map(|s| s.encode_hex::<String>())
        .collect::<Vec<String>>();

    Ok(signers)
}

#[derive(RlpEncodable, RlpDecodable)]
struct PackedSignature {
    pub v: u8,
    pub r: ethereum_types::U256,
    pub s: ethereum_types::U256,
}

pub fn encode_signature_rlp(signature: &Signature) -> BytesMut {
    let mut signature_bytes = BytesMut::new();
    PackedSignature {
        v: signature.v.try_into().unwrap(),
        r: signature.r,
        s: signature.s,
    }
    .encode(&mut signature_bytes);
    signature_bytes
}

pub fn decode_signature_rlp(
    signature_bytes: &mut &[u8],
) -> Result<Signature, open_fastrlp::DecodeError> {
    let packed_signature = PackedSignature::decode(signature_bytes)?;
    Ok(Signature {
        r: packed_signature.r,
        s: packed_signature.s,
        v: packed_signature.v.into(),
    })
}
