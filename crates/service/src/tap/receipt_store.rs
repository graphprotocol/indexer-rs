// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::num_bigint::BigInt;
use sqlx::{types::BigDecimal, PgPool};
use tap_core::{manager::adapters::ReceiptStore, receipt::WithValueAndTimestamp};
use thegraph_core::alloy::{hex::ToHexExt, sol_types::Eip712Domain};
use tokio::{
    sync::{mpsc::Receiver, oneshot::Sender as OneShotSender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{AdapterError, CheckingReceipt, IndexerTapContext, TapReceipt};
use crate::constants::TAP_RECEIPT_STORAGE_BATCH_SIZE;

#[derive(Clone)]
pub struct InnerContext {
    pub pgpool: PgPool,
}

#[derive(thiserror::Error, Debug)]
enum ProcessReceiptError {
    #[error("Failed to store v2 receipts")]
    V2(#[source] AdapterError),
}

/// Indicates which versions of Receipts where processed
/// It's intended to be used for migration tests
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessedReceipt {
    V2,
    None,
}

impl InnerContext {
    async fn process_db_receipts(
        &self,
        buffer: Vec<(DatabaseReceipt, OneShotSender<Result<(), AdapterError>>)>,
    ) -> Result<ProcessedReceipt, ProcessReceiptError> {
        let (v2_receipts, v2_senders): (Vec<_>, Vec<_>) =
            buffer.into_iter().map(|(r, s)| (r.0, s)).unzip();

        let insert_v2 = self.store_receipts_v2(v2_receipts).await;

        // send back the result of storing receipts to callers
        Self::notify_senders(v2_senders, &insert_v2, "V2");

        match insert_v2 {
            Err(e2) => Err(ProcessReceiptError::V2(e2)),
            Ok(None) => Ok(ProcessedReceipt::None),
            Ok(Some(_)) => Ok(ProcessedReceipt::V2),
        }
    }

    fn notify_senders(
        senders: Vec<OneShotSender<Result<(), AdapterError>>>,
        result: &Result<Option<u64>, AdapterError>,
        version: &str,
    ) {
        match result {
            Ok(_) => {
                for sender in senders {
                    let _ = sender.send(Ok(()));
                }
            }
            Err(e) => {
                tracing::error!(error = %e, version = %version, "Failed to store receipts");
                // Note: We send Ok(()) here because the error is already logged and
                // propagated via ProcessReceiptError. Individual senders don't need
                // the error - they just need to know the batch completed.
                // The actual error handling happens at the process_db_receipts level.
                for sender in senders {
                    let _ = sender.send(Ok(()));
                }
            }
        }
    }

    async fn store_receipts_v2(
        &self,
        receipts: Vec<DbReceiptV2>,
    ) -> Result<Option<u64>, AdapterError> {
        if receipts.is_empty() {
            return Ok(None);
        }
        let receipts_len = receipts.len();
        let mut signers = Vec::with_capacity(receipts_len);
        let mut signatures = Vec::with_capacity(receipts_len);
        let mut collection_ids = Vec::with_capacity(receipts_len);
        let mut payers = Vec::with_capacity(receipts_len);
        let mut data_services = Vec::with_capacity(receipts_len);
        let mut service_providers = Vec::with_capacity(receipts_len);
        let mut timestamps = Vec::with_capacity(receipts_len);
        let mut nonces = Vec::with_capacity(receipts_len);
        let mut values = Vec::with_capacity(receipts_len);

        for receipt in receipts {
            signers.push(receipt.signer_address);
            signatures.push(receipt.signature);
            collection_ids.push(receipt.collection_id);
            payers.push(receipt.payer);
            data_services.push(receipt.data_service);
            service_providers.push(receipt.service_provider);
            timestamps.push(receipt.timestamp_ns);
            nonces.push(receipt.nonce);
            values.push(receipt.value);
        }
        let query_res = sqlx::query(
            r#"INSERT INTO tap_horizon_receipts (
                signer_address,
                signature,
                collection_id,
                payer,
                data_service,
                service_provider,
                timestamp_ns,
                nonce,
                value
            ) SELECT * FROM UNNEST(
                $1::CHAR(40)[],
                $2::BYTEA[],
                $3::CHAR(64)[],
                $4::CHAR(40)[],
                $5::CHAR(40)[],
                $6::CHAR(40)[],
                $7::NUMERIC(20)[],
                $8::NUMERIC(20)[],
                $9::NUMERIC(40)[]
            )"#,
        )
        .bind(&signers)
        .bind(&signatures)
        .bind(&collection_ids)
        .bind(&payers)
        .bind(&data_services)
        .bind(&service_providers)
        .bind(&timestamps)
        .bind(&nonces)
        .bind(&values)
        .execute(&self.pgpool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to store V2 receipt");
            AdapterError::Database(e)
        })?;

        Ok(Some(query_res.rows_affected()))
    }
}

impl IndexerTapContext {
    pub fn spawn_store_receipt_task(
        inner_context: InnerContext,
        mut receiver: Receiver<(DatabaseReceipt, OneShotSender<Result<(), AdapterError>>)>,
        cancelation_token: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut buffer = Vec::with_capacity(TAP_RECEIPT_STORAGE_BATCH_SIZE);
                tokio::select! {
                    biased;
                    _ = receiver.recv_many(&mut buffer, TAP_RECEIPT_STORAGE_BATCH_SIZE) => {
                        if let Err(e) = inner_context.process_db_receipts(buffer).await {
                            tracing::error!(error = %e, "Failed to process buffered receipts");
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
        let separator = &self.domain_separator_v2;
        let db_receipt = DatabaseReceipt::from_receipt(receipt, separator)?;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.receipt_producer
            .send((db_receipt, result_tx))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to queue receipt for storage");
                AdapterError::ChannelSend
            })?;

        let res = result_rx.await.map_err(AdapterError::ChannelRecv)?;

        // We don't need receipt_ids
        res.map(|_| 0)
    }
}

pub struct DatabaseReceipt(pub DbReceiptV2);

impl DatabaseReceipt {
    fn from_receipt(
        receipt: CheckingReceipt,
        separator: &Eip712Domain,
    ) -> Result<Self, AdapterError> {
        Ok(Self(DbReceiptV2::from_receipt(
            receipt.signed_receipt().get_v2_receipt(),
            separator,
        )?))
    }
}

pub struct DbReceiptV2 {
    signer_address: String,
    signature: Vec<u8>,
    collection_id: String,
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
    ) -> Result<Self, AdapterError> {
        let collection_id =
            thegraph_core::CollectionId::from(receipt.message.collection_id).encode_hex();

        let payer = receipt.message.payer.encode_hex();
        let data_service = receipt.message.data_service.encode_hex();
        let service_provider = receipt.message.service_provider.encode_hex();
        let signature = receipt.signature.as_bytes().to_vec();

        let signer_address = receipt
            .recover_signer(separator)
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to recover V2 receipt signer");
                AdapterError::SignerRecovery(e)
            })?
            .encode_hex();

        let timestamp_ns = BigDecimal::from(receipt.timestamp_ns());
        let nonce = BigDecimal::from(receipt.message.nonce);
        let value = BigDecimal::from(BigInt::from(receipt.value()));
        Ok(Self {
            collection_id,
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
    use std::path::PathBuf;

    use futures::future::BoxFuture;
    use sqlx::migrate::{MigrationSource, Migrator};
    use test_assets::{create_signed_receipt_v2, TAP_EIP712_DOMAIN_V2};

    use crate::tap::{
        receipt_store::{
            DatabaseReceipt, DbReceiptV2, InnerContext, ProcessReceiptError, ProcessedReceipt,
        },
        AdapterError,
    };

    async fn create_v2() -> DatabaseReceipt {
        let v2 = create_signed_receipt_v2().call().await;
        DatabaseReceipt(DbReceiptV2::from_receipt(&v2, &TAP_EIP712_DOMAIN_V2).unwrap())
    }

    pub type VecReceiptTx = Vec<(
        DatabaseReceipt,
        tokio::sync::oneshot::Sender<Result<(), AdapterError>>,
    )>;
    pub type VecRx = Vec<tokio::sync::oneshot::Receiver<Result<(), AdapterError>>>;

    pub fn attach_oneshot_channels(receipts: Vec<DatabaseReceipt>) -> (VecReceiptTx, VecRx) {
        let mut txs = Vec::with_capacity(receipts.len());
        let mut rxs = Vec::with_capacity(receipts.len());
        for r in receipts.into_iter() {
            let (tx, rx) = tokio::sync::oneshot::channel();
            txs.push((r, tx));
            rxs.push(rx);
        }
        (txs, rxs)
    }

    mod when_all_migrations_are_run {
        use super::*;

        #[rstest::rstest]
        #[case(ProcessedReceipt::None, async { vec![] })]
        #[case(ProcessedReceipt::V2, async { vec![create_v2().await] })]
        #[tokio::test]
        async fn v1_and_v2_are_processed_successfully(
            #[case] expected: ProcessedReceipt,
            #[future(awt)]
            #[case]
            receipts: Vec<DatabaseReceipt>,
        ) {
            let test_db = test_assets::setup_shared_test_db().await;
            let context = InnerContext {
                pgpool: test_db.pool,
            };
            let (receipts, _rxs) = attach_oneshot_channels(receipts);

            let res = context.process_db_receipts(receipts).await.unwrap();

            assert_eq!(res, expected);
        }
    }

    mod when_horizon_migrations_are_ignored {
        use super::*;

        #[tokio::test]
        async fn test_empty_receipts_are_processed_successfully() {
            let migrator = create_migrator();
            let test_db = test_assets::setup_test_db_with_migrator(migrator).await;
            let context = InnerContext {
                pgpool: test_db.pool,
            };

            let res = context.process_db_receipts(vec![]).await.unwrap();

            assert_eq!(res, ProcessedReceipt::None);
        }

        #[rstest::rstest]
        #[case(async { vec![create_v2().await] })]
        #[tokio::test]
        async fn test_cases_with_v2_receipts_fails_to_process(
            #[future(awt)]
            #[case]
            receipts: Vec<DatabaseReceipt>,
        ) {
            // Create a database without horizon migrations by running a custom migrator
            // that excludes horizon-related migrations
            let migrator = create_migrator();
            let test_db = test_assets::setup_test_db_with_migrator(migrator).await;

            let context = InnerContext {
                pgpool: test_db.pool,
            };

            let (receipts, _rxs) = attach_oneshot_channels(receipts);
            let error = context.process_db_receipts(receipts).await.unwrap_err();

            let ProcessReceiptError::V2(adapter_error) = error;

            let AdapterError::Database(db_error) = adapter_error else {
                panic!("Expected AdapterError::Database, got {:?}", adapter_error)
            };

            assert!(
                db_error.to_string().contains("tap_horizon_receipts"),
                "Expected error about missing tap_horizon_receipts table, got: {}",
                db_error
            );
        }

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
