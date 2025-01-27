// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use bigdecimal::num_bigint::BigInt;
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{
    manager::adapters::ReceiptStore,
    receipt::{state::Checking, ReceiptWithState, SignedReceipt},
};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{AdapterError, IndexerTapContext};

#[derive(Clone)]
pub struct InnerContext {
    pub pgpool: PgPool,
}

impl InnerContext {
    async fn store_receipts(&self, receipts: Vec<DatabaseReceipt>) -> Result<(), AdapterError> {
        let receipts_len = receipts.len();
        let mut signers = Vec::with_capacity(receipts_len);
        let mut signatures = Vec::with_capacity(receipts_len);
        let mut allocation_ids = Vec::with_capacity(receipts_len);
        let mut timestamps = Vec::with_capacity(receipts_len);
        let mut nonces = Vec::with_capacity(receipts_len);
        let mut values = Vec::with_capacity(receipts_len);

        for receipt in receipts {
            signers.push(receipt.signer_address);
            signatures.push(receipt.signature);
            allocation_ids.push(receipt.allocation_id);
            timestamps.push(receipt.timestamp_ns);
            nonces.push(receipt.nonce);
            values.push(receipt.value);
        }
        sqlx::query!(
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
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to store receipt: {}", e);
            anyhow!(e)
        })?;

        Ok(())
    }
}

impl IndexerTapContext {
    pub fn spawn_store_receipt_task(
        inner_context: InnerContext,
        mut receiver: Receiver<DatabaseReceipt>,
        cancelation_token: CancellationToken,
    ) -> JoinHandle<()> {
        const BUFFER_SIZE: usize = 100;
        tokio::spawn(async move {
            loop {
                let mut buffer = Vec::with_capacity(BUFFER_SIZE);
                tokio::select! {
                    biased;
                    _ = receiver.recv_many(&mut buffer, BUFFER_SIZE) => {
                        if let Err(e) = inner_context.store_receipts(buffer).await {
                            tracing::error!("Failed to store receipts: {}", e);
                        }
                    }
                    _ = cancelation_token.cancelled() => { break },
                }
            }
        })
    }
}

#[async_trait::async_trait]
impl ReceiptStore<SignedReceipt> for IndexerTapContext {
    type AdapterError = AdapterError;

    async fn store_receipt(
        &self,
        receipt: ReceiptWithState<Checking, SignedReceipt>,
    ) -> Result<u64, Self::AdapterError> {
        let db_receipt = DatabaseReceipt::from_receipt(receipt, &self.domain_separator)?;
        self.receipt_producer.send(db_receipt).await.map_err(|e| {
            tracing::error!("Failed to queue receipt for storage: {}", e);
            anyhow!(e)
        })?;

        // We don't need receipt_ids
        Ok(0)
    }
}

pub struct DatabaseReceipt {
    signer_address: String,
    signature: Vec<u8>,
    allocation_id: String,
    timestamp_ns: BigDecimal,
    nonce: BigDecimal,
    value: BigDecimal,
}

impl DatabaseReceipt {
    fn from_receipt(
        receipt: ReceiptWithState<Checking, SignedReceipt>,
        separator: &Eip712Domain,
    ) -> anyhow::Result<Self> {
        let receipt = receipt.signed_receipt();
        let allocation_id = receipt.message.allocation_id.encode_hex();
        let signature = receipt.signature.as_bytes().to_vec();

        let signer_address = receipt
            .recover_signer(separator)
            .map_err(|e| {
                tracing::error!("Failed to recover receipt signer: {}", e);
                anyhow!(e)
            })?
            .encode_hex();

        let timestamp_ns = BigDecimal::from(receipt.message.timestamp_ns);
        let nonce = BigDecimal::from(receipt.message.nonce);
        let value = BigDecimal::from(BigInt::from(receipt.message.value));
        Ok(Self {
            allocation_id,
            nonce,
            signature,
            signer_address,
            timestamp_ns,
            value,
        })
    }
}
