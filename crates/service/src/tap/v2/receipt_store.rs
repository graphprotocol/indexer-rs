// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use bigdecimal::num_bigint::BigInt;
use sqlx::{types::BigDecimal, PgPool};
use tap_core_v2::{
    manager::adapters::ReceiptStore,
    receipt::{state::Checking, ReceiptWithState},
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
        let mut payers = Vec::with_capacity(receipts_len);
        let mut data_services = Vec::with_capacity(receipts_len);
        let mut service_providers = Vec::with_capacity(receipts_len);
        let mut timestamps = Vec::with_capacity(receipts_len);
        let mut nonces = Vec::with_capacity(receipts_len);
        let mut values = Vec::with_capacity(receipts_len);

        for receipt in receipts {
            signers.push(receipt.signer_address);
            signatures.push(receipt.signature);
            payers.push(receipt.payer);
            data_services.push(receipt.data_service);
            service_providers.push(receipt.service_provider);
            timestamps.push(receipt.timestamp_ns);
            nonces.push(receipt.nonce);
            values.push(receipt.value);
        }
        sqlx::query!(
            r#"INSERT INTO tap_v2_receipts (
                signer_address,
                signature,
                payer,
                data_service,
                service_provider,
                timestamp_ns,
                nonce,
                value
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(40)[],
                $4::CHAR(40)[],
                $5::CHAR(40)[],
                $6::NUMERIC(20)[],
                $7::NUMERIC(20)[],
                $8::NUMERIC(40)[]
            )"#,
            &signers,
            &signatures,
            &payers,
            &data_services,
            &service_providers,
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
impl ReceiptStore for IndexerTapContext {
    type AdapterError = AdapterError;

    async fn store_receipt(
        &self,
        receipt: ReceiptWithState<Checking>,
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
    payer: String,
    data_service: String,
    service_provider: String,
    timestamp_ns: BigDecimal,
    nonce: BigDecimal,
    value: BigDecimal,
}

impl DatabaseReceipt {
    fn from_receipt(
        receipt: ReceiptWithState<Checking>,
        separator: &Eip712Domain,
    ) -> anyhow::Result<Self> {
        let receipt = receipt.signed_receipt();
        let payer = receipt.message.payer.encode_hex();
        let data_service = receipt.message.data_service.encode_hex();
        let service_provider = receipt.message.service_provider.encode_hex();
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
            nonce,
            signature,
            signer_address,
            timestamp_ns,
            value,
            payer,
            data_service,
            service_provider,
        })
    }
}
