// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    num::TryFromIntError,
    ops::{Bound, RangeBounds},
    str::FromStr,
};

use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
use indexer_receipt::TapReceipt;
use sqlx::{postgres::types::PgRange, types::BigDecimal};
use tap_core::manager::adapters::{safe_truncate_receipts, ReceiptDelete, ReceiptRead};
use tap_graph::{Receipt, SignedReceipt};
use thegraph_core::{
    alloy::{
        hex::ToHexExt,
        primitives::{Address, FixedBytes},
    },
    CollectionId,
};

use super::{error::AdapterError, Horizon, Legacy, TapAgentContext};
use crate::tap::{signers_trimmed, CheckingReceipt};
impl From<TryFromIntError> for AdapterError {
    fn from(error: TryFromIntError) -> Self {
        AdapterError::ReceiptRead {
            error: error.to_string(),
        }
    }
}

impl From<sqlx::Error> for AdapterError {
    fn from(error: sqlx::Error) -> Self {
        AdapterError::ReceiptRead {
            error: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for AdapterError {
    fn from(error: serde_json::Error) -> Self {
        AdapterError::ReceiptRead {
            error: error.to_string(),
        }
    }
}

/// convert Bound`<u64>` to Bound`<BigDecimal>`
#[inline]
fn u64_bound_to_bigdecimal_bound(bound: Bound<&u64>) -> Bound<BigDecimal> {
    match bound {
        Bound::Included(val) => Bound::Included(BigDecimal::from(*val)),
        Bound::Excluded(val) => Bound::Excluded(BigDecimal::from(*val)),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// convert RangeBounds`<u64>` to PgRange`<BigDecimal>`
fn rangebounds_to_pgrange<R: RangeBounds<u64>>(range: R) -> PgRange<BigDecimal> {
    // Test for empty ranges. Because the PG range type does not behave the same as
    // Rust's range type when start > end.
    if match (range.start_bound(), range.end_bound()) {
        (Bound::Included(start), Bound::Included(end)) => start > end,
        (Bound::Included(start), Bound::Excluded(end)) => start >= end,
        (Bound::Excluded(start), Bound::Included(end)) => start >= end,
        (Bound::Excluded(start), Bound::Excluded(end)) => start >= end || *start == end - 1,
        _ => false,
    } {
        // Return an empty PG range.
        return PgRange::<BigDecimal>::from(BigDecimal::from(0)..BigDecimal::from(0));
    }
    PgRange::<BigDecimal>::from((
        u64_bound_to_bigdecimal_bound(range.start_bound()),
        u64_bound_to_bigdecimal_bound(range.end_bound()),
    ))
}

/// Implements a [ReceiptRead] for [TapReceipt]
/// in case [super::NetworkVersion] is [Legacy]
///
/// This is important because receipts for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl ReceiptRead<TapReceipt> for TapAgentContext<Legacy> {
    type AdapterError = AdapterError;

    async fn retrieve_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_range_ns: R,
        receipts_limit: Option<u64>,
    ) -> Result<Vec<CheckingReceipt>, Self::AdapterError> {
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender)
            .await
            .map_err(|e| AdapterError::ReceiptRead {
                error: format!("{:?}.", e),
            })?;

        let receipts_limit = receipts_limit.map_or(1000, |limit| limit);

        let records = sqlx::query!(
            r#"
                SELECT id, signature, allocation_id, timestamp_ns, nonce, value
                FROM scalar_tap_receipts
                WHERE allocation_id = $1 AND signer_address IN (SELECT unnest($2::text[]))
                AND $3::numrange @> timestamp_ns
                ORDER BY timestamp_ns ASC
                LIMIT $4
            "#,
            self.allocation_id.encode_hex(),
            &signers,
            rangebounds_to_pgrange(timestamp_range_ns),
            (receipts_limit + 1) as i64,
        )
        .fetch_all(&self.pgpool)
        .await?;
        let mut receipts = records
            .into_iter()
            .map(|record| {
                let signature = record.signature.as_slice().try_into()
                    .map_err(|e| AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding signature while retrieving receipt from database: {}",
                            e
                        ),
                    })?;
                let allocation_id = Address::from_str(&record.allocation_id).map_err(|e| {
                    AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding allocation_id while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;
                let timestamp_ns = record
                    .timestamp_ns
                    .to_u64()
                    .ok_or(AdapterError::ReceiptRead {
                        error: "Error decoding timestamp_ns while retrieving receipt from database"
                            .to_string(),
                    })?;
                let nonce = record.nonce.to_u64().ok_or(AdapterError::ReceiptRead {
                    error: "Error decoding nonce while retrieving receipt from database".to_string(),
                })?;
                // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                // So we're converting to BigInt to get a proper implementation of to_u128().
                let value = record.value.to_bigint().and_then(|v| v.to_u128()).ok_or(AdapterError::ReceiptRead {
                    error: "Error decoding value while retrieving receipt from database".to_string(),
                })?;

                let signed_receipt = SignedReceipt {
                    message: Receipt {
                        allocation_id,
                        timestamp_ns,
                        nonce,
                        value,
                    },
                    signature,
                };

                Ok(CheckingReceipt::new(TapReceipt::V1(signed_receipt)))

            })
            .collect::<Result<Vec<_>, AdapterError>>()?;

        safe_truncate_receipts(&mut receipts, receipts_limit);

        Ok(receipts)
    }
}

/// Implements a [ReceiptDelete] for [TapReceipt]
/// in case [super::NetworkVersion] is [Legacy]
///
/// This is important because receipts for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl ReceiptDelete for TapAgentContext<Legacy> {
    type AdapterError = AdapterError;

    async fn remove_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_ns: R,
    ) -> Result<(), Self::AdapterError> {
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender)
            .await
            .map_err(|e| AdapterError::ReceiptDelete {
                error: format!("{:?}.", e),
            })?;

        sqlx::query!(
            r#"
                DELETE FROM scalar_tap_receipts
                WHERE allocation_id = $1 AND signer_address IN (SELECT unnest($2::text[]))
                    AND $3::numrange @> timestamp_ns
            "#,
            self.allocation_id.encode_hex(),
            &signers,
            rangebounds_to_pgrange(timestamp_ns)
        )
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }
}

/// Implements a [ReceiptRead] for [TapReceipt]
/// in case [super::NetworkVersion] is [Horizon]
///
/// This is important because receipts for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl ReceiptRead<TapReceipt> for TapAgentContext<Horizon> {
    type AdapterError = AdapterError;

    async fn retrieve_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_range_ns: R,
        receipts_limit: Option<u64>,
    ) -> Result<Vec<CheckingReceipt>, Self::AdapterError> {
        let receipts_limit = receipts_limit.map_or(1000, |limit| limit);

        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender)
            .await
            .map_err(|e| AdapterError::ReceiptRead {
                error: format!("{:?}.", e),
            })?;

        // TODO filter by data_service when we have multiple data services

        let records = sqlx::query!(
            r#"
                SELECT 
                    id,
                    signature,
                    collection_id,
                    payer,
                    data_service,
                    service_provider,
                    timestamp_ns,
                    nonce,
                    value
                FROM tap_horizon_receipts
                WHERE
                    collection_id = $1
                    AND payer = $2
                    AND service_provider = $3
                    AND signer_address IN (SELECT unnest($4::text[]))
                AND $5::numrange @> timestamp_ns
                ORDER BY timestamp_ns ASC
                LIMIT $6
            "#,
            CollectionId::from(self.allocation_id).encode_hex(),
            self.sender.encode_hex(),
            self.indexer_address.encode_hex(),
            &signers,
            rangebounds_to_pgrange(timestamp_range_ns),
            (receipts_limit + 1) as i64,
        )
        .fetch_all(&self.pgpool)
        .await?;
        let mut receipts = records
            .into_iter()
            .map(|record| {
                let signature = record.signature.as_slice().try_into()
                    .map_err(|e| AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding signature while retrieving receipt from database: {}",
                            e
                        ),
                    })?;
                let collection_id = FixedBytes::<32>::from_str(&record.collection_id).map_err(|e| {
                    AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding collection_id while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;
                let payer = Address::from_str(&record.payer).map_err(|e| {
                    AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding payer while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;

                let data_service = Address::from_str(&record.data_service).map_err(|e| {
                    AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding data_service while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;

                let service_provider = Address::from_str(&record.service_provider).map_err(|e| {
                    AdapterError::ReceiptRead {
                        error: format!(
                            "Error decoding service_provider while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;

                let timestamp_ns = record
                    .timestamp_ns
                    .to_u64()
                    .ok_or(AdapterError::ReceiptRead {
                        error: "Error decoding timestamp_ns while retrieving receipt from database"
                            .to_string(),
                    })?;
                let nonce = record.nonce.to_u64().ok_or(AdapterError::ReceiptRead {
                    error: "Error decoding nonce while retrieving receipt from database".to_string(),
                })?;
                // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                // So we're converting to BigInt to get a proper implementation of to_u128().
                let value = record.value.to_bigint().and_then(|v| v.to_u128()).ok_or(AdapterError::ReceiptRead {
                    error: "Error decoding value while retrieving receipt from database".to_string(),
                })?;

                let signed_receipt = tap_graph::v2::SignedReceipt {
                    message: tap_graph::v2::Receipt {
                        payer,
                        data_service,
                        service_provider,
                        collection_id,
                        timestamp_ns,
                        nonce,
                        value,
                    },
                    signature,
                };

                Ok(CheckingReceipt::new(TapReceipt::V2(signed_receipt)))

            })
            .collect::<Result<Vec<_>, AdapterError>>()?;

        safe_truncate_receipts(&mut receipts, receipts_limit);

        Ok(receipts)
    }
}

/// Implements a [ReceiptDelete] for [TapReceipt]
/// in case [super::NetworkVersion] is [Horizon]
///
/// This is important because receipts for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl ReceiptDelete for TapAgentContext<Horizon> {
    type AdapterError = AdapterError;

    async fn remove_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_ns: R,
    ) -> Result<(), Self::AdapterError> {
        let signers = signers_trimmed(self.escrow_accounts.clone(), self.sender)
            .await
            .map_err(|e| AdapterError::ReceiptDelete {
                error: format!("{:?}.", e),
            })?;

        sqlx::query!(
            r#"
                DELETE FROM tap_horizon_receipts
                WHERE
                    collection_id = $1
                    AND signer_address IN (SELECT unnest($2::text[]))
                    AND $3::numrange @> timestamp_ns
                    AND payer = $4
                    AND service_provider = $5
            "#,
            CollectionId::from(self.allocation_id).encode_hex(),
            &signers,
            rangebounds_to_pgrange(timestamp_ns),
            self.sender.encode_hex(),
            self.indexer_address.encode_hex(),
        )
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::{Bound, HashMap},
        ops::RangeBounds,
        str::FromStr,
        sync::LazyLock,
    };

    use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
    use indexer_monitor::EscrowAccounts;
    use rstest::{fixture, rstest};
    use sqlx::PgPool;
    use tap_core::{
        manager::adapters::{ReceiptDelete, ReceiptRead},
        receipt::{WithUniqueId, WithValueAndTimestamp},
    };
    use test_assets::{
        ALLOCATION_ID_0, ALLOCATION_ID_1, TAP_EIP712_DOMAIN as TAP_EIP712_DOMAIN_SEPARATOR,
        TAP_SENDER as SENDER, TAP_SIGNER as SIGNER,
    };
    use thegraph_core::alloy::{
        primitives::{Address, U256},
        signers::local::PrivateKeySigner,
    };
    use tokio::sync::watch::{self, Receiver};

    use super::*;
    use crate::test::{store_receipt, CreateReceipt, SENDER_2};

    const ALLOCATION_ID_IRRELEVANT: Address = ALLOCATION_ID_1;

    static SENDER_IRRELEVANT: LazyLock<(PrivateKeySigner, Address)> =
        LazyLock::new(|| SENDER_2.clone());

    #[fixture]
    fn escrow_accounts() -> Receiver<EscrowAccounts> {
        watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1
    }

    async fn legacy_adapter(
        pgpool: PgPool,
        escrow_accounts: Receiver<EscrowAccounts>,
    ) -> TapAgentContext<Legacy> {
        TapAgentContext::builder()
            .pgpool(pgpool)
            .escrow_accounts(escrow_accounts)
            .build()
    }

    async fn horizon_adapter(
        pgpool: PgPool,
        escrow_accounts: Receiver<EscrowAccounts>,
    ) -> TapAgentContext<Horizon> {
        TapAgentContext::builder()
            .pgpool(pgpool)
            .escrow_accounts(escrow_accounts)
            .build()
    }

    /// Insert a single receipt and retrieve it from the database using the adapter.
    /// The point here it to test the deserialization of large numbers.
    #[rstest]
    #[case(legacy_adapter(_pgpool.clone(), _escrow.clone()))]
    #[case(horizon_adapter(_pgpool.clone(), _escrow.clone()))]
    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_and_retrieve_single_receipt<T>(
        #[ignore] _pgpool: PgPool,
        #[from(escrow_accounts)] _escrow: Receiver<EscrowAccounts>,
        #[case]
        #[future(awt)]
        context: TapAgentContext<T>,
    ) where
        T: CreateReceipt<Id = Address>,
        TapAgentContext<T>: ReceiptRead<TapReceipt> + ReceiptDelete,
    {
        let received_receipt =
            T::create_received_receipt(ALLOCATION_ID_0, &SIGNER.0, u64::MAX, u64::MAX, u128::MAX);

        // Storing the receipt
        store_receipt(&context.pgpool, received_receipt.signed_receipt())
            .await
            .unwrap();

        let retrieved_receipt = context
            .retrieve_receipts_in_timestamp_range(.., None)
            .await
            .unwrap()[0]
            .clone();

        let received_id = received_receipt.signed_receipt().unique_id();
        let retrieved_id = retrieved_receipt.signed_receipt().unique_id();
        assert_eq!(received_id, retrieved_id);
    }

    /// This function compares a local receipts vector filter by timestamp range (we assume that the stdlib
    /// implementation is correct) with the receipts vector retrieved from the database using
    /// retrieve_receipts_in_timestamp_range.
    async fn retrieve_range_and_check<R: RangeBounds<u64> + Send, T>(
        storage_adapter: &TapAgentContext<T>,
        escrow_accounts: Receiver<EscrowAccounts>,
        received_receipt_vec: &[CheckingReceipt],
        range: R,
    ) -> anyhow::Result<()>
    where
        TapAgentContext<T>: ReceiptRead<TapReceipt>,
    {
        let escrow_accounts_snapshot = escrow_accounts.borrow();

        // Filtering the received receipts by timestamp range
        let received_receipt_vec: Vec<_> = received_receipt_vec
            .iter()
            .filter(|received_receipt| {
                use thegraph_core::CollectionId;
                let expected_collection_id = *CollectionId::from(storage_adapter.allocation_id);

                let id_matches = received_receipt.signed_receipt().allocation_id()
                    == Some(storage_adapter.allocation_id)
                    || received_receipt.signed_receipt().collection_id()
                        == Some(expected_collection_id);

                range.contains(&received_receipt.signed_receipt().timestamp_ns())
                    && id_matches
                    && escrow_accounts_snapshot
                        .get_sender_for_signer(
                            &received_receipt
                                .signed_receipt()
                                .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                                .unwrap(),
                        )
                        .is_ok_and(|v| v == storage_adapter.sender)
            })
            .cloned()
            .collect();

        // Retrieving receipts in timestamp range from the database, convert to json Value
        let recovered_received_receipt_vec = storage_adapter
            .retrieve_receipts_in_timestamp_range(range, None)
            .await?
            .into_iter()
            .map(|r| r.signed_receipt().unique_id())
            .collect::<Vec<_>>();

        // Check length
        assert_eq!(
            recovered_received_receipt_vec.len(),
            received_receipt_vec.len()
        );

        // Checking that the receipts in recovered_received_receipt_vec are the same as
        // the ones in received_receipt_vec
        assert!(received_receipt_vec.iter().all(|received_receipt| {
            recovered_received_receipt_vec.contains(&received_receipt.signed_receipt().unique_id())
        }));
        Ok(())
    }

    trait RemoveRange: Sized {
        async fn remove_range_and_check<R: RangeBounds<u64> + Send>(
            storage_adapter: &TapAgentContext<Self>,
            escrow_accounts: Receiver<EscrowAccounts>,
            received_receipt_vec: &[CheckingReceipt],
            range: R,
        ) -> anyhow::Result<()>;
    }

    impl RemoveRange for Horizon {
        async fn remove_range_and_check<R: RangeBounds<u64> + Send>(
            storage_adapter: &TapAgentContext<Self>,
            escrow_accounts: Receiver<EscrowAccounts>,
            received_receipt_vec: &[CheckingReceipt],
            range: R,
        ) -> anyhow::Result<()> {
            let escrow_accounts_snapshot = escrow_accounts.borrow();

            // Storing the receipts
            let mut received_receipt_id_vec = Vec::new();
            for received_receipt in received_receipt_vec.iter() {
                received_receipt_id_vec.push(
                    store_receipt(&storage_adapter.pgpool, received_receipt.signed_receipt())
                        .await
                        .unwrap(),
                );
            }

            // zip the 2 vectors together
            let received_receipt_vec = received_receipt_id_vec
                .into_iter()
                .zip(received_receipt_vec.iter())
                .collect::<Vec<_>>();

            // Remove the received receipts by timestamp range for the correct (collection_id,
            // sender)
            let received_receipt_vec: Vec<_> = received_receipt_vec
                .iter()
                .filter(|(_, received_receipt)| {
                    use thegraph_core::CollectionId;
                    let expected_collection_id = *CollectionId::from(storage_adapter.allocation_id);
                    if (received_receipt.signed_receipt().collection_id()
                        == Some(expected_collection_id))
                        && escrow_accounts_snapshot
                            .get_sender_for_signer(
                                &received_receipt
                                    .signed_receipt()
                                    .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                                    .unwrap(),
                            )
                            .is_ok_and(|v| v == storage_adapter.sender)
                    {
                        !range.contains(&received_receipt.signed_receipt().timestamp_ns())
                    } else {
                        true
                    }
                    // !range.contains(&received_receipt.signed_receipt().message.timestamp_ns)
                })
                .cloned()
                .collect();

            // Removing the received receipts in timestamp range from the database
            storage_adapter
                .remove_receipts_in_timestamp_range(range)
                .await?;

            // Retrieving all receipts in DB (including irrelevant ones)
            let records = sqlx::query!(
                r#"
                SELECT 
                    signature,
                    collection_id,
                    payer,
                    data_service,
                    service_provider,
                    timestamp_ns,
                    nonce,
                    value
                FROM tap_horizon_receipts
            "#
            )
            .fetch_all(&storage_adapter.pgpool)
            .await?;

            // Check length
            assert_eq!(records.len(), received_receipt_vec.len());

            // Retrieving all receipts in DB (including irrelevant ones)
            let recovered_received_receipt_set: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let signature = record.signature.as_slice().try_into().unwrap();
                    let collection_id = FixedBytes::<32>::from_str(&record.collection_id).unwrap();
                    let payer = Address::from_str(&record.payer).unwrap();
                    let data_service = Address::from_str(&record.data_service).unwrap();
                    let service_provider = Address::from_str(&record.service_provider).unwrap();
                    let timestamp_ns = record.timestamp_ns.to_u64().unwrap();
                    let nonce = record.nonce.to_u64().unwrap();
                    // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                    // So we're converting to BigInt to get a proper implementation of to_u128().
                    let value = record
                        .value
                        .to_bigint()
                        .map(|v| v.to_u128())
                        .unwrap()
                        .unwrap();

                    let signed_receipt = tap_graph::v2::SignedReceipt {
                        message: tap_graph::v2::Receipt {
                            collection_id,
                            payer,
                            data_service,
                            service_provider,
                            timestamp_ns,
                            nonce,
                            value,
                        },
                        signature,
                    };
                    signed_receipt.unique_id()
                })
                .collect();

            // Check values recovered_received_receipt_set contains values received_receipt_vec
            assert!(received_receipt_vec.iter().all(|(_, received_receipt)| {
                recovered_received_receipt_set
                    .contains(&received_receipt.signed_receipt().unique_id())
            }));

            // Removing all the receipts in the DB
            sqlx::query!(
                r#"
                DELETE FROM tap_horizon_receipts
            "#
            )
            .execute(&storage_adapter.pgpool)
            .await?;

            // Checking that there are no receipts left
            let scalar_tap_receipts_db_count: i64 = sqlx::query!(
                r#"
                SELECT count(*)
                FROM tap_horizon_receipts
            "#
            )
            .fetch_one(&storage_adapter.pgpool)
            .await?
            .count
            .unwrap();
            assert_eq!(scalar_tap_receipts_db_count, 0);
            Ok(())
        }
    }

    impl RemoveRange for Legacy {
        async fn remove_range_and_check<R: RangeBounds<u64> + Send>(
            storage_adapter: &TapAgentContext<Self>,
            escrow_accounts: Receiver<EscrowAccounts>,
            received_receipt_vec: &[CheckingReceipt],
            range: R,
        ) -> anyhow::Result<()> {
            let escrow_accounts_snapshot = escrow_accounts.borrow();

            // Storing the receipts
            let mut received_receipt_id_vec = Vec::new();
            for received_receipt in received_receipt_vec.iter() {
                received_receipt_id_vec.push(
                    store_receipt(&storage_adapter.pgpool, received_receipt.signed_receipt())
                        .await
                        .unwrap(),
                );
            }

            // zip the 2 vectors together
            let received_receipt_vec = received_receipt_id_vec
                .into_iter()
                .zip(received_receipt_vec.iter())
                .collect::<Vec<_>>();

            // Remove the received receipts by timestamp range for the correct (allocation_id,
            // sender)
            let received_receipt_vec: Vec<_> = received_receipt_vec
                .iter()
                .filter(|(_, received_receipt)| {
                    if (received_receipt.signed_receipt().allocation_id()
                        == Some(storage_adapter.allocation_id))
                        && escrow_accounts_snapshot
                            .get_sender_for_signer(
                                &received_receipt
                                    .signed_receipt()
                                    .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                                    .unwrap(),
                            )
                            .is_ok_and(|v| v == storage_adapter.sender)
                    {
                        !range.contains(&received_receipt.signed_receipt().timestamp_ns())
                    } else {
                        true
                    }
                    // !range.contains(&received_receipt.signed_receipt().message.timestamp_ns)
                })
                .cloned()
                .collect();

            // Removing the received receipts in timestamp range from the database
            storage_adapter
                .remove_receipts_in_timestamp_range(range)
                .await?;

            // Retrieving all receipts in DB (including irrelevant ones)
            let records = sqlx::query!(
                r#"
                SELECT signature, allocation_id, timestamp_ns, nonce, value
                FROM scalar_tap_receipts
            "#
            )
            .fetch_all(&storage_adapter.pgpool)
            .await?;

            // Check length
            assert_eq!(records.len(), received_receipt_vec.len());

            // Retrieving all receipts in DB (including irrelevant ones)
            let recovered_received_receipt_set: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let signature = record.signature.as_slice().try_into().unwrap();
                    let allocation_id = Address::from_str(&record.allocation_id).unwrap();
                    let timestamp_ns = record.timestamp_ns.to_u64().unwrap();
                    let nonce = record.nonce.to_u64().unwrap();
                    // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood...
                    // So we're converting to BigInt to get a proper implementation of to_u128().
                    let value = record
                        .value
                        .to_bigint()
                        .map(|v| v.to_u128())
                        .unwrap()
                        .unwrap();

                    let signed_receipt = SignedReceipt {
                        message: Receipt {
                            allocation_id,
                            timestamp_ns,
                            nonce,
                            value,
                        },
                        signature,
                    };
                    signed_receipt.unique_id()
                })
                .collect();

            // Check values recovered_received_receipt_set contains values received_receipt_vec
            assert!(received_receipt_vec.iter().all(|(_, received_receipt)| {
                recovered_received_receipt_set
                    .contains(&received_receipt.signed_receipt().unique_id())
            }));

            // Removing all the receipts in the DB
            sqlx::query!(
                r#"
                DELETE FROM scalar_tap_receipts
            "#
            )
            .execute(&storage_adapter.pgpool)
            .await?;

            // Checking that there are no receipts left
            let scalar_tap_receipts_db_count: i64 = sqlx::query!(
                r#"
                SELECT count(*)
                FROM scalar_tap_receipts
            "#
            )
            .fetch_one(&storage_adapter.pgpool)
            .await?
            .count
            .unwrap();
            assert_eq!(scalar_tap_receipts_db_count, 0);
            Ok(())
        }
    }

    #[rstest]
    #[case(legacy_adapter(_pgpool.clone(), _escrow.clone()))]
    #[case(horizon_adapter(_pgpool.clone(), _escrow.clone()))]
    #[sqlx::test(migrations = "../../migrations")]
    async fn retrieve_receipts_with_limit<T>(
        #[ignore] _pgpool: PgPool,
        #[from(escrow_accounts)] _escrow: Receiver<EscrowAccounts>,
        #[case]
        #[future(awt)]
        context: TapAgentContext<T>,
    ) where
        T: CreateReceipt<Id = Address>,
        TapAgentContext<T>: ReceiptRead<TapReceipt> + ReceiptDelete,
    {
        // Creating 100 receipts with timestamps 42 to 141
        for i in 0..100 {
            let receipt = T::create_received_receipt(
                ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            );
            store_receipt(&context.pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let recovered_received_receipt_vec = context
            .retrieve_receipts_in_timestamp_range(0..141, Some(10))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 10);

        let recovered_received_receipt_vec = context
            .retrieve_receipts_in_timestamp_range(0..141, Some(50))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 50);

        // add a copy in the same timestamp
        for i in 0..100 {
            let receipt = T::create_received_receipt(
                ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 43,
                (i + 124).into(),
            );
            store_receipt(&context.pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let recovered_received_receipt_vec = context
            .retrieve_receipts_in_timestamp_range(0..141, Some(10))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 9);

        let recovered_received_receipt_vec = context
            .retrieve_receipts_in_timestamp_range(0..141, Some(50))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 49);
    }

    #[rstest]
    #[case(legacy_adapter(pgpool.clone(), escrow_accounts.clone()))]
    #[case(horizon_adapter(pgpool.clone(), escrow_accounts.clone()))]
    #[sqlx::test(migrations = "../../migrations")]
    async fn retrieve_receipts_in_timestamp_range<T>(
        #[ignore] pgpool: PgPool,
        #[from(escrow_accounts)] escrow_accounts: Receiver<EscrowAccounts>,
        #[case]
        #[future(awt)]
        context: TapAgentContext<T>,
    ) where
        T: CreateReceipt<Id = Address>,
        TapAgentContext<T>: ReceiptRead<TapReceipt> + ReceiptDelete,
    {
        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_IRRELEVANT,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_0,
                &SENDER_IRRELEVANT.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
        }

        // Storing the receipts
        let mut received_receipt_id_vec = Vec::new();
        for received_receipt in received_receipt_vec.iter() {
            received_receipt_id_vec.push(
                store_receipt(&pgpool, received_receipt.signed_receipt())
                    .await
                    .unwrap(),
            );
        }

        macro_rules! test_ranges{
            ($($arg: expr), +) => {
                {
                    $(
                        assert!(
                        retrieve_range_and_check(&context, escrow_accounts.clone(), &received_receipt_vec, $arg)
                            .await
                            .is_ok());
                    )+
                }
            };
        }

        #[allow(clippy::reversed_empty_ranges)]
        {
            test_ranges!(
                ..,
                ..41,
                ..42,
                ..43,
                ..50,
                ..51,
                ..52,
                ..=41,
                ..=42,
                ..=43,
                ..=50,
                ..=51,
                ..=52,
                21..=41,
                21..=42,
                21..=43,
                21..=50,
                21..=51,
                21..=52,
                41..=41,
                41..=42,
                41..=43,
                41..=50,
                50..=48,
                41..=51,
                41..=52,
                51..=51,
                51..=52,
                21..41,
                21..42,
                21..43,
                21..50,
                21..51,
                21..52,
                41..41,
                41..42,
                41..43,
                41..50,
                50..48,
                41..51,
                41..52,
                51..51,
                51..52,
                41..,
                42..,
                43..,
                50..,
                51..,
                52..,
                (Bound::Excluded(42), Bound::Excluded(43)),
                (Bound::Excluded(43), Bound::Excluded(43)),
                (Bound::Excluded(43), Bound::Excluded(44)),
                (Bound::Excluded(43), Bound::Excluded(45))
            );
        }
    }

    #[rstest]
    #[case(legacy_adapter(_pgpool.clone(), escrow_accounts.clone()))]
    #[case(horizon_adapter(_pgpool.clone(), escrow_accounts.clone()))]
    #[sqlx::test(migrations = "../../migrations")]
    async fn remove_receipts_in_timestamp_range<T>(
        #[ignore] _pgpool: PgPool,
        #[from(escrow_accounts)] escrow_accounts: Receiver<EscrowAccounts>,
        #[case]
        #[future(awt)]
        context: TapAgentContext<T>,
    ) where
        T: CreateReceipt<Id = Address> + RemoveRange,
        TapAgentContext<T>: ReceiptRead<TapReceipt> + ReceiptDelete,
    {
        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_IRRELEVANT,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
            received_receipt_vec.push(T::create_received_receipt(
                ALLOCATION_ID_0,
                &SENDER_IRRELEVANT.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
        }

        macro_rules! test_ranges{
            ($($arg: expr), +) => {
                {
                    $(
                        assert!(
                            T::remove_range_and_check(&context, escrow_accounts.clone(), &received_receipt_vec, $arg)
                            .await.is_ok()
                        );
                    ) +
                }
            };
        }

        #[allow(clippy::reversed_empty_ranges)]
        {
            test_ranges!(
                ..,
                ..41,
                ..42,
                ..43,
                ..50,
                ..51,
                ..52,
                ..=41,
                ..=42,
                ..=43,
                ..=50,
                ..=51,
                ..=52,
                21..=41,
                21..=42,
                21..=43,
                21..=50,
                21..=51,
                21..=52,
                41..=41,
                41..=42,
                41..=43,
                41..=50,
                50..=48,
                41..=51,
                41..=52,
                51..=51,
                51..=52,
                21..41,
                21..42,
                21..43,
                21..50,
                21..51,
                21..52,
                41..41,
                41..42,
                41..43,
                41..50,
                50..48,
                41..51,
                41..52,
                51..51,
                51..52,
                41..,
                42..,
                43..,
                50..,
                51..,
                52..,
                (Bound::Excluded(42), Bound::Excluded(43)),
                (Bound::Excluded(43), Bound::Excluded(43)),
                (Bound::Excluded(43), Bound::Excluded(44)),
                (Bound::Excluded(43), Bound::Excluded(45))
            );
        }
    }
}
