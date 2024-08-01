// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::hex::ToHex;
use anyhow::anyhow;
use bigdecimal::num_bigint::BigInt;
use sqlx::types::BigDecimal;
use tap_core::{
    manager::adapters::ReceiptStore,
    receipt::{state::Checking, ReceiptWithState},
};
use tracing::error;

use super::{AdapterError, IndexerTapContext};

#[async_trait::async_trait]
impl ReceiptStore for IndexerTapContext {
    type AdapterError = AdapterError;

    async fn store_receipt(
        &self,
        receipt: ReceiptWithState<Checking>,
    ) -> Result<u64, Self::AdapterError> {
        let receipt = receipt.signed_receipt();
        let allocation_id = receipt.message.allocation_id;
        let encoded_signature = receipt.signature.to_vec();

        let receipt_signer = receipt
            .recover_signer(self.domain_separator.as_ref())
            .map_err(|e| {
                error!("Failed to recover receipt signer: {}", e);
                anyhow!(e)
            })?;

        // TODO: consider doing this in another async task to avoid slowing down the paid query flow.
        sqlx::query!(
            r#"
                INSERT INTO scalar_tap_receipts (signer_address, signature, allocation_id, timestamp_ns, nonce, value)
                VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            receipt_signer.encode_hex::<String>(),
            encoded_signature,
            allocation_id.encode_hex::<String>(),
            BigDecimal::from(receipt.message.timestamp_ns),
            BigDecimal::from(receipt.message.nonce),
            BigDecimal::from(BigInt::from(receipt.message.value)),
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            error!("Failed to store receipt: {}", e);
            anyhow!(e)
        })?;

        // We don't need receipt_ids
        Ok(0)
    }
}
