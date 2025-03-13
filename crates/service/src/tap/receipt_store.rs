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

#[derive(thiserror::Error, Debug)]
enum ProcessReceiptError {
    #[error("Failed to store v1 receipts: {0}")]
    V1(anyhow::Error),
    #[error("Failed to store v2 receipts: {0}")]
    V2(anyhow::Error),
    #[error("Failed to receipts for v1 and v2. Error v1: {0}. Error v2: {1}")]
    Both(anyhow::Error, anyhow::Error),
}

/// Indicates which versions of Receipts where processed
/// It's intended to be used for migration tests
#[derive(Debug, PartialEq, Eq)]
pub enum Processed {
    V1,
    V2,
    All,
    None,
}

impl InnerContext {
    async fn process_db_receipts(
        &self,
        buffer: Vec<DatabaseReceipt>,
    ) -> Result<Processed, ProcessReceiptError> {
        let (v1_receipts, v2_receipts): (Vec<_>, Vec<_>) =
            buffer.into_iter().partition_map(|r| match r {
                DatabaseReceipt::V1(db_receipt_v1) => Either::Left(db_receipt_v1),
                DatabaseReceipt::V2(db_receipt_v2) => Either::Right(db_receipt_v2),
            });

        let (insert_v1, insert_v2) = match (v1_receipts.is_empty(), v2_receipts.is_empty()) {
            (true, true) => (None, None),
            (false, true) => (Some(self.store_receipts_v1(v1_receipts).await), None),
            (true, false) => (None, Some(self.store_receipts_v2(v2_receipts).await)),
            (false, false) => {
                let (v1, v2) = tokio::join!(
                    self.store_receipts_v1(v1_receipts),
                    self.store_receipts_v2(v2_receipts),
                );
                (Some(v1), Some(v2))
            }
        };

        match (insert_v1, insert_v2) {
            (Some(Err(e1)), Some(Err(e2))) => Err(ProcessReceiptError::Both(e1.into(), e2.into())),
            (Some(Err(e1)), _) => Err(ProcessReceiptError::V1(e1.into())),
            (_, Some(Err(e2))) => Err(ProcessReceiptError::V2(e2.into())),

            // only useful for testing
            (Some(Ok(_)), None) => Ok(Processed::V1),
            (None, Some(Ok(_))) => Ok(Processed::V2),
            (Some(Ok(_)), Some(Ok(_))) => Ok(Processed::All),
            (None, None) => Ok(Processed::None),
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
                        if let Err(e) = inner_context.process_db_receipts(buffer).await {
                            tracing::error!("{e}");
                        }
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

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::LazyLock};

    use futures::future::BoxFuture;
    use sqlx::{
        migrate::{MigrationSource, Migrator},
        PgPool,
    };
    use test_assets::{
        create_signed_receipt, create_signed_receipt_v2, SignedReceiptRequest, INDEXER_ALLOCATIONS,
        TAP_EIP712_DOMAIN,
    };

    use crate::tap::{
        receipt_store::{
            DatabaseReceipt, DbReceiptV1, DbReceiptV2, InnerContext, ProcessReceiptError, Processed,
        },
        AdapterError,
    };

    async fn create_v1() -> DatabaseReceipt {
        let alloc = INDEXER_ALLOCATIONS.values().next().unwrap().clone();
        let v1 = create_signed_receipt(
            SignedReceiptRequest::builder()
                .allocation_id(alloc.id)
                .value(100)
                .build(),
        )
        .await;
        DatabaseReceipt::V1(DbReceiptV1::from_receipt(&v1, &TAP_EIP712_DOMAIN).unwrap())
    }

    async fn create_v2() -> DatabaseReceipt {
        let v2 = create_signed_receipt_v2().call().await;
        DatabaseReceipt::V2(DbReceiptV2::from_receipt(&v2, &TAP_EIP712_DOMAIN).unwrap())
    }

    mod when_all_migrations_are_run {
        use super::*;

        #[rstest::rstest]
        #[case(Processed::None, async { vec![] })]
        #[case(Processed::V1, async { vec![create_v1().await] })]
        #[case(Processed::V2, async { vec![create_v2().await] })]
        #[case(Processed::All, async { vec![create_v2().await, create_v1().await] })]
        #[sqlx::test(migrations = "../../migrations")]
        async fn v1_and_v2_are_processed_successfully(
            #[ignore] pgpool: PgPool,
            #[case] expected: Processed,
            #[future(awt)]
            #[case]
            receipts: Vec<DatabaseReceipt>,
        ) {
            let context = InnerContext { pgpool };

            let res = context.process_db_receipts(receipts).await.unwrap();

            assert_eq!(res, expected);
        }
    }

    mod when_horizon_migrations_are_ignored {
        use super::*;

        #[sqlx::test(migrator = "WITHOUT_HORIZON_MIGRATIONS")]
        async fn test_empty_receipts_are_processed_successfully(pgpool: PgPool) {
            let context = InnerContext { pgpool };

            let res = context.process_db_receipts(vec![]).await.unwrap();

            assert_eq!(res, Processed::None);
        }

        #[sqlx::test(migrator = "WITHOUT_HORIZON_MIGRATIONS")]
        async fn test_v1_receipts_are_processed_successfully(pgpool: PgPool) {
            let context = InnerContext { pgpool };

            let v1 = create_v1().await;
            let receipts = vec![v1];

            let res = context.process_db_receipts(receipts).await.unwrap();

            assert_eq!(res, Processed::V1);
        }

        #[rstest::rstest]
        #[case(async { vec![create_v2().await] })]
        #[case(async { vec![create_v2().await, create_v1().await] })]
        #[sqlx::test(migrator = "WITHOUT_HORIZON_MIGRATIONS")]
        async fn test_cases_with_v2_receipts_fails_to_process(
            #[ignore] pgpool: PgPool,
            #[future(awt)]
            #[case]
            receipts: Vec<DatabaseReceipt>,
        ) {
            let context = InnerContext { pgpool };

            let error = context.process_db_receipts(receipts).await.unwrap_err();

            let ProcessReceiptError::V2(error) = error else {
                panic!()
            };
            let d = error.downcast_ref::<AdapterError>().unwrap().to_string();

            assert_eq!(
                d,
                "error returned from database: relation \"tap_horizon_receipts\" does not exist"
            );
        }

        pub static WITHOUT_HORIZON_MIGRATIONS: LazyLock<Migrator> = LazyLock::new(create_migrator);

        pub fn create_migrator() -> Migrator {
            futures::executor::block_on(Migrator::new(MigrationRunner::new(
                "../../migrations",
                ["horizon"],
            )))
            .unwrap()
        }

        #[derive(Debug)]
        pub struct MigrationRunner {
            migration_path: PathBuf,
            ignored_migrations: Vec<String>,
        }

        impl MigrationRunner {
            /// Construct a new MigrationRunner that does not apply the given migrations.
            ///
            /// `ignored_migrations` is any iterable of strings that describes which
            /// migrations to be ignored.
            pub fn new<I>(path: impl Into<PathBuf>, ignored_migrations: I) -> Self
            where
                I: IntoIterator,
                I::Item: Into<String>,
            {
                Self {
                    migration_path: path.into(),
                    ignored_migrations: ignored_migrations.into_iter().map(Into::into).collect(),
                }
            }
        }

        impl MigrationSource<'static> for MigrationRunner {
            fn resolve(
                self,
            ) -> BoxFuture<'static, Result<Vec<sqlx::migrate::Migration>, sqlx::error::BoxDynError>>
            {
                Box::pin(async move {
                    let canonical = self.migration_path.canonicalize()?;
                    let migrations_with_paths =
                        sqlx::migrate::resolve_blocking(&canonical).unwrap();

                    let migrations_with_paths = migrations_with_paths
                        .into_iter()
                        .filter(|(_, p)| {
                            let path = p.to_str().unwrap();
                            self.ignored_migrations
                                .iter()
                                .any(|ignored| !path.contains(ignored))
                        })
                        .collect::<Vec<_>>();

                    Ok(migrations_with_paths.into_iter().map(|(m, _p)| m).collect())
                })
            }
        }
    }
}
