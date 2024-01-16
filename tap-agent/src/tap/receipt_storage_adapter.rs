// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    num::TryFromIntError,
    ops::{Bound, RangeBounds},
};

use alloy_primitives::Address;
use async_trait::async_trait;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use sqlx::{postgres::types::PgRange, types::BigDecimal, PgPool};
use tap_core::adapters::receipt_storage_adapter::ReceiptStorageAdapter as ReceiptStorageAdapterTrait;
use tap_core::{
    tap_manager::SignedReceipt,
    tap_receipt::{ReceiptCheck, ReceivedReceipt},
};
use thiserror::Error;
use tracing::error;

use crate::tap::signers_trimmed;

pub struct ReceiptStorageAdapter {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
    required_checks: Vec<ReceiptCheck>,
    escrow_accounts: Eventual<EscrowAccounts>,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Error in ReceiptStorageAdapter: {error}")]
    AdapterError { error: String },
}

impl From<TryFromIntError> for AdapterError {
    fn from(error: TryFromIntError) -> Self {
        AdapterError::AdapterError {
            error: error.to_string(),
        }
    }
}

impl From<sqlx::Error> for AdapterError {
    fn from(error: sqlx::Error) -> Self {
        AdapterError::AdapterError {
            error: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for AdapterError {
    fn from(error: serde_json::Error) -> Self {
        AdapterError::AdapterError {
            error: error.to_string(),
        }
    }
}

/// convert Bound`<u64>` to Bound`<BigDecimal>`
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

#[async_trait]
impl ReceiptStorageAdapterTrait for ReceiptStorageAdapter {
    type AdapterError = AdapterError;

    /// We don't need this method in TAP Agent.
    async fn store_receipt(&self, _receipt: ReceivedReceipt) -> Result<u64, Self::AdapterError> {
        unimplemented!();
    }

    async fn retrieve_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_range_ns: R,
        // TODO: Make use of this limit in this function
        _receipts_limit: Option<u64>,
    ) -> Result<Vec<(u64, ReceivedReceipt)>, Self::AdapterError> {
        let signers = signers_trimmed(&self.escrow_accounts, self.sender)
            .await
            .map_err(|e| AdapterError::AdapterError {
                error: format!("{:?}.", e),
            })?;

        let records = sqlx::query!(
            r#"
                SELECT id, receipt
                FROM scalar_tap_receipts
                WHERE allocation_id = $1 AND signer_address IN (SELECT unnest($2::text[]))
                 AND $3::numrange @> timestamp_ns
            "#,
            self.allocation_id
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            &signers,
            rangebounds_to_pgrange(timestamp_range_ns)
        )
        .fetch_all(&self.pgpool)
        .await?;
        records
            .into_iter()
            .map(|record| {
                let id: u64 = record.id.try_into()?;
                let signed_receipt: SignedReceipt = serde_json::from_value(record.receipt)?;
                let received_receipt =
                    ReceivedReceipt::new(signed_receipt, id, &self.required_checks);
                Ok((id, received_receipt))
            })
            .collect()
    }

    /// We don't need this method in TAP Agent.
    async fn update_receipt_by_id(
        &self,
        _receipt_id: u64,
        _receipt: ReceivedReceipt,
    ) -> Result<(), Self::AdapterError> {
        unimplemented!();
    }

    async fn remove_receipts_in_timestamp_range<R: RangeBounds<u64> + Send>(
        &self,
        timestamp_ns: R,
    ) -> Result<(), Self::AdapterError> {
        let signers = signers_trimmed(&self.escrow_accounts, self.sender)
            .await
            .map_err(|e| AdapterError::AdapterError {
                error: format!("{:?}.", e),
            })?;

        sqlx::query!(
            r#"
                DELETE FROM scalar_tap_receipts
                WHERE allocation_id = $1 AND signer_address IN (SELECT unnest($2::text[]))
                    AND $3::numrange @> timestamp_ns
            "#,
            self.allocation_id
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            &signers,
            rangebounds_to_pgrange(timestamp_ns)
        )
        .execute(&self.pgpool)
        .await?;
        Ok(())
    }
}

impl ReceiptStorageAdapter {
    pub fn new(
        pgpool: PgPool,
        allocation_id: Address,
        sender: Address,
        required_checks: Vec<ReceiptCheck>,
        escrow_accounts: Eventual<EscrowAccounts>,
    ) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
            required_checks,
            escrow_accounts,
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use super::*;
    use crate::tap::test_utils::{
        create_received_receipt, store_receipt, ALLOCATION_ID, ALLOCATION_ID_IRRELEVANT, SENDER,
        SENDER_IRRELEVANT, SIGNER, TAP_EIP712_DOMAIN_SEPARATOR,
    };
    use anyhow::Result;

    use serde_json::Value;
    use sqlx::PgPool;
    use tap_core::tap_receipt::get_full_list_of_checks;

    /// This function compares a local receipts vector filter by timestamp range (we assume that the stdlib
    /// implementation is correct) with the receipts vector retrieved from the database using
    /// retrieve_receipts_in_timestamp_range.
    async fn retrieve_range_and_check<R: RangeBounds<u64> + Send>(
        storage_adapter: &ReceiptStorageAdapter,
        escrow_accounts: &Eventual<EscrowAccounts>,
        received_receipt_vec: &[(u64, ReceivedReceipt)],
        range: R,
    ) -> Result<()> {
        let escrow_accounts_snapshot = escrow_accounts.value().await.unwrap();

        // Filtering the received receipts by timestamp range
        let received_receipt_vec: Vec<(u64, ReceivedReceipt)> = received_receipt_vec
            .iter()
            .filter(|(_, received_receipt)| {
                range.contains(&received_receipt.signed_receipt().message.timestamp_ns)
                    && (received_receipt.signed_receipt().message.allocation_id
                        == storage_adapter.allocation_id)
                    && (escrow_accounts_snapshot
                        .get_sender_for_signer(
                            &received_receipt
                                .signed_receipt()
                                .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                                .unwrap(),
                        )
                        .map_or(false, |v| v == storage_adapter.sender))
            })
            .cloned()
            .collect();

        // Retrieving receipts in timestamp range from the database, convert to json Value
        let recovered_received_receipt_vec = storage_adapter
            // TODO: Make use of the receipt limit if it makes sense here
            .retrieve_receipts_in_timestamp_range(range, None)
            .await?
            .iter()
            .map(|(id, received_receipt)| {
                let mut received_receipt = serde_json::to_value(received_receipt).unwrap();

                // We set all query_id to 0 because we don't care about it in this test and it
                // makes it easier.
                received_receipt["query_id"] = Value::from(0u64);
                (*id, received_receipt)
            })
            .collect::<Vec<(u64, Value)>>();

        // Check length
        assert_eq!(
            recovered_received_receipt_vec.len(),
            received_receipt_vec.len()
        );

        // Checking that the receipts in recovered_received_receipt_vec are the same as
        // the ones in received_receipt_vec
        assert!(received_receipt_vec.iter().all(|(id, received_receipt)| {
            recovered_received_receipt_vec
                .contains(&(*id, serde_json::to_value(received_receipt).unwrap()))
        }));
        Ok(())
    }

    async fn remove_range_and_check<R: RangeBounds<u64> + Send>(
        storage_adapter: &ReceiptStorageAdapter,
        escrow_accounts: &Eventual<EscrowAccounts>,
        received_receipt_vec: &[ReceivedReceipt],
        range: R,
    ) -> Result<()> {
        let escrow_accounts_snapshot = escrow_accounts.value().await.unwrap();

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
        let received_receipt_vec: Vec<(u64, &ReceivedReceipt)> = received_receipt_vec
            .iter()
            .filter(|(_, received_receipt)| {
                if (received_receipt.signed_receipt().message.allocation_id
                    == storage_adapter.allocation_id)
                    && (escrow_accounts_snapshot
                        .get_sender_for_signer(
                            &received_receipt
                                .signed_receipt()
                                .recover_signer(&TAP_EIP712_DOMAIN_SEPARATOR)
                                .unwrap(),
                        )
                        .map_or(false, |v| v == storage_adapter.sender))
                {
                    !range.contains(&received_receipt.signed_receipt().message.timestamp_ns)
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
                SELECT receipt
                FROM scalar_tap_receipts
            "#
        )
        .fetch_all(&storage_adapter.pgpool)
        .await?;

        // Check length
        assert_eq!(records.len(), received_receipt_vec.len());

        // Retrieving all receipts in DB (including irrelevant ones)
        let recovered_received_receipt_set: Vec<Value> = records
            .into_iter()
            .map(|record| {
                let signed_receipt: SignedReceipt = serde_json::from_value(record.receipt).unwrap();
                let reveived_receipt =
                    ReceivedReceipt::new(signed_receipt, 0, &get_full_list_of_checks());
                serde_json::to_value(reveived_receipt).unwrap()
            })
            .collect();

        // Check values recovered_received_receipt_set contains values received_receipt_vec
        assert!(received_receipt_vec.iter().all(|(_, received_receipt)| {
            recovered_received_receipt_set
                .contains(&serde_json::to_value(received_receipt).unwrap())
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

    #[sqlx::test(migrations = "../migrations")]
    async fn retrieve_receipts_in_timestamp_range(pgpool: PgPool) {
        let escrow_accounts = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let storage_adapter = ReceiptStorageAdapter::new(
            pgpool.clone(),
            *ALLOCATION_ID,
            SENDER.1,
            get_full_list_of_checks(),
            escrow_accounts.clone(),
        );

        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID,
                    &SIGNER.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID_IRRELEVANT,
                    &SIGNER.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID,
                    &SENDER_IRRELEVANT.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );
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

        // zip the 2 vectors together
        let received_receipt_vec = received_receipt_id_vec
            .into_iter()
            .zip(received_receipt_vec.into_iter())
            .collect::<Vec<_>>();

        macro_rules! test_ranges{
            ($($arg: expr), +) => {
                {
                    $(
                        assert!(
                        retrieve_range_and_check(&storage_adapter, &escrow_accounts, &received_receipt_vec, $arg)
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

    #[sqlx::test(migrations = "../migrations")]
    async fn remove_receipts_in_timestamp_range(pgpool: PgPool) {
        let escrow_accounts = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let storage_adapter = ReceiptStorageAdapter::new(
            pgpool,
            *ALLOCATION_ID,
            SENDER.1,
            get_full_list_of_checks(),
            escrow_accounts.clone(),
        );

        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID,
                    &SIGNER.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID_IRRELEVANT,
                    &SIGNER.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );
            received_receipt_vec.push(
                create_received_receipt(
                    &ALLOCATION_ID,
                    &SENDER_IRRELEVANT.0,
                    i + 684,
                    i + 42,
                    (i + 124).into(),
                    0,
                )
                .await,
            );
        }

        macro_rules! test_ranges{
            ($($arg: expr), +) => {
                {
                    $(
                        assert!(
                            remove_range_and_check(&storage_adapter, &escrow_accounts, &received_receipt_vec, $arg)
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
