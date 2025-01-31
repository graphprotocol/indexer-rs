// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use bigdecimal::num_bigint::BigInt;
use itertools::{Either, Itertools};
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{manager::adapters::ReceiptStore, receipt::WithValueAndTimestamp};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{AdapterError, CheckingReceipt, IndexerTapContext, TapReceipt};

#[derive(Clone)]
pub struct InnerContext {
    pub pgpool: PgPool,
}

impl InnerContext {
    async fn process_db_receipts(&self, buffer: Vec<DatabaseReceipt>) {
        let (v1_receipts, v2_receipts): (Vec<_>, Vec<_>) =
            buffer.into_iter().partition_map(|r| match r {
                DatabaseReceipt::V1(db_receipt_v1) => Either::Left(db_receipt_v1),
                DatabaseReceipt::V2(db_receipt_v2) => Either::Right(db_receipt_v2),
            });
        let (insert_v1, insert_v2) = tokio::join!(
            self.store_receipts_v1(v1_receipts),
            self.store_receipts_v2(v2_receipts)
        );
        if let Err(e) = insert_v1 {
            tracing::error!("Failed to store v1 receipts: {}", e);
        }
        if let Err(e) = insert_v2 {
            tracing::error!("Failed to store v2 receipts: {}", e);
        }
    }

    async fn store_receipts_v1(&self, receipts: Vec<DbReceiptV1>) -> Result<(), AdapterError> {
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

    async fn store_receipts_v2(&self, receipts: Vec<DbReceiptV2>) -> Result<(), AdapterError> {
        let receipts_len = receipts.len();
        let mut signers = Vec::with_capacity(receipts_len);
        let mut signatures = Vec::with_capacity(receipts_len);
        let mut allocation_ids = Vec::with_capacity(receipts_len);
        let mut payers = Vec::with_capacity(receipts_len);
        let mut data_services = Vec::with_capacity(receipts_len);
        let mut service_providers = Vec::with_capacity(receipts_len);
        let mut timestamps = Vec::with_capacity(receipts_len);
        let mut nonces = Vec::with_capacity(receipts_len);
        let mut values = Vec::with_capacity(receipts_len);

        for receipt in receipts {
            signers.push(receipt.signer_address);
            signatures.push(receipt.signature);
            allocation_ids.push(receipt.allocation_id);
            payers.push(receipt.payer);
            data_services.push(receipt.data_service);
            service_providers.push(receipt.service_provider);
            timestamps.push(receipt.timestamp_ns);
            nonces.push(receipt.nonce);
            values.push(receipt.value);
        }
        sqlx::query!(
            r#"INSERT INTO tap_horizon_receipts (
                signer_address,
                signature,
                allocation_id,
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
                $6::CHAR(40)[],
                $7::NUMERIC(20)[],
                $8::NUMERIC(20)[],
                $9::NUMERIC(40)[]
            )"#,
            &signers,
            &signatures,
            &allocation_ids,
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
                        inner_context.process_db_receipts(buffer).await;
                    }
                    _ = cancelation_token.cancelled() => { break },
                }
            }
        })
    }
}

#[async_trait::async_trait]
impl ReceiptStore<TapReceipt> for IndexerTapContext {
    type AdapterError = AdapterError;

    async fn store_receipt(&self, receipt: CheckingReceipt) -> Result<u64, Self::AdapterError> {
        let db_receipt = DatabaseReceipt::from_receipt(receipt, &self.domain_separator)?;
        self.receipt_producer.send(db_receipt).await.map_err(|e| {
            tracing::error!("Failed to queue receipt for storage: {}", e);
            anyhow!(e)
        })?;

        // We don't need receipt_ids
        Ok(0)
    }
}

pub enum DatabaseReceipt {
    V1(DbReceiptV1),
    V2(DbReceiptV2),
}

impl DatabaseReceipt {
    fn from_receipt(receipt: CheckingReceipt, separator: &Eip712Domain) -> anyhow::Result<Self> {
        Ok(match receipt.signed_receipt() {
            TapReceipt::V1(receipt) => Self::V1(DbReceiptV1::from_receipt(receipt, separator)?),
            TapReceipt::V2(receipt) => Self::V2(DbReceiptV2::from_receipt(receipt, separator)?),
        })
    }
}

pub struct DbReceiptV1 {
    signer_address: String,
    signature: Vec<u8>,
    allocation_id: String,
    timestamp_ns: BigDecimal,
    nonce: BigDecimal,
    value: BigDecimal,
}

impl DbReceiptV1 {
    fn from_receipt(
        receipt: &tap_graph::SignedReceipt,
        separator: &Eip712Domain,
    ) -> anyhow::Result<Self> {
        let allocation_id = receipt.message.allocation_id.encode_hex();
        let signature = receipt.signature.as_bytes().to_vec();

        let signer_address = receipt
            .recover_signer(separator)
            .map_err(|e| {
                tracing::error!("Failed to recover receipt signer: {}", e);
                anyhow!(e)
            })?
            .encode_hex();

        let timestamp_ns = BigDecimal::from(receipt.timestamp_ns());
        let nonce = BigDecimal::from(receipt.message.nonce);
        let value = BigDecimal::from(BigInt::from(receipt.value()));
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

pub struct DbReceiptV2 {
    signer_address: String,
    signature: Vec<u8>,
    allocation_id: String,
    payer: String,
    data_service: String,
    service_provider: String,
    timestamp_ns: BigDecimal,
    nonce: BigDecimal,
    value: BigDecimal,
}

impl DbReceiptV2 {
    fn from_receipt(
        receipt: &tap_graph::v2::SignedReceipt,
        separator: &Eip712Domain,
    ) -> anyhow::Result<Self> {
        let allocation_id = receipt.message.allocation_id.encode_hex();
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

        let timestamp_ns = BigDecimal::from(receipt.timestamp_ns());
        let nonce = BigDecimal::from(receipt.message.nonce);
        let value = BigDecimal::from(BigInt::from(receipt.value()));
        Ok(Self {
            allocation_id,
            payer,
            data_service,
            service_provider,
            nonce,
            signature,
            signer_address,
            timestamp_ns,
            value,
        })
    }
}
