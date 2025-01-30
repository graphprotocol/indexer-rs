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
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};

use super::{error::AdapterError, TapAgentContext};
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

#[async_trait::async_trait]
impl ReceiptRead<TapReceipt> for TapAgentContext {
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

#[async_trait::async_trait]
impl ReceiptDelete for TapAgentContext {
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

#[cfg(test)]
mod test {
    use std::{
        collections::{Bound, HashMap},
        ops::RangeBounds,
        str::FromStr,
    };

    use bigdecimal::{num_bigint::ToBigInt, ToPrimitive};
    use indexer_monitor::EscrowAccounts;
    use lazy_static::lazy_static;
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
    use crate::test::{create_received_receipt, store_receipt, SENDER_2};

    const ALLOCATION_ID_IRRELEVANT: Address = ALLOCATION_ID_1;

    lazy_static! {
        static ref SENDER_IRRELEVANT: (PrivateKeySigner, Address) = SENDER_2.clone();
    }

    /// Insert a single receipt and retrieve it from the database using the adapter.
    /// The point here it to test the deserialization of large numbers.
    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_and_retrieve_single_receipt(pgpool: PgPool) {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let storage_adapter =
            TapAgentContext::new(pgpool, ALLOCATION_ID_0, SENDER.1, escrow_accounts.clone());

        let received_receipt =
            create_received_receipt(&ALLOCATION_ID_0, &SIGNER.0, u64::MAX, u64::MAX, u128::MAX);

        // Storing the receipt
        store_receipt(&storage_adapter.pgpool, received_receipt.signed_receipt())
            .await
            .unwrap();

        let retrieved_receipt = storage_adapter
            .retrieve_receipts_in_timestamp_range(.., None)
            .await
            .unwrap()[0]
            .clone();

        assert_eq!(
            received_receipt.signed_receipt().unique_id(),
            retrieved_receipt.signed_receipt().unique_id(),
        );
    }

    /// This function compares a local receipts vector filter by timestamp range (we assume that the stdlib
    /// implementation is correct) with the receipts vector retrieved from the database using
    /// retrieve_receipts_in_timestamp_range.
    async fn retrieve_range_and_check<R: RangeBounds<u64> + Send>(
        storage_adapter: &TapAgentContext,
        escrow_accounts: Receiver<EscrowAccounts>,
        received_receipt_vec: &[CheckingReceipt],
        range: R,
    ) -> anyhow::Result<()> {
        let escrow_accounts_snapshot = escrow_accounts.borrow();

        // Filtering the received receipts by timestamp range
        let received_receipt_vec: Vec<_> = received_receipt_vec
            .iter()
            .filter(|received_receipt| {
                range.contains(&received_receipt.signed_receipt().timestamp_ns())
                    && (received_receipt.signed_receipt().allocation_id()
                        == storage_adapter.allocation_id)
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

    async fn remove_range_and_check<R: RangeBounds<u64> + Send>(
        storage_adapter: &TapAgentContext,
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
                    == storage_adapter.allocation_id)
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
            recovered_received_receipt_set.contains(&received_receipt.signed_receipt().unique_id())
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn retrieve_receipts_with_limit(pgpool: PgPool) {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let storage_adapter = TapAgentContext::new(
            pgpool.clone(),
            ALLOCATION_ID_0,
            SENDER.1,
            escrow_accounts.clone(),
        );

        // Creating 100 receipts with timestamps 42 to 141
        for i in 0..100 {
            let receipt = create_received_receipt(
                &ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            );
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let recovered_received_receipt_vec = storage_adapter
            .retrieve_receipts_in_timestamp_range(0..141, Some(10))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 10);

        let recovered_received_receipt_vec = storage_adapter
            .retrieve_receipts_in_timestamp_range(0..141, Some(50))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 50);

        // add a copy in the same timestamp
        for i in 0..100 {
            let receipt = create_received_receipt(
                &ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 43,
                (i + 124).into(),
            );
            store_receipt(&pgpool, receipt.signed_receipt())
                .await
                .unwrap();
        }

        let recovered_received_receipt_vec = storage_adapter
            .retrieve_receipts_in_timestamp_range(0..141, Some(10))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 9);

        let recovered_received_receipt_vec = storage_adapter
            .retrieve_receipts_in_timestamp_range(0..141, Some(50))
            .await
            .unwrap();
        assert_eq!(recovered_received_receipt_vec.len(), 49);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn retrieve_receipts_in_timestamp_range(pgpool: PgPool) {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let storage_adapter = TapAgentContext::new(
            pgpool.clone(),
            ALLOCATION_ID_0,
            SENDER.1,
            escrow_accounts.clone(),
        );

        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_IRRELEVANT,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_0,
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

        // zip the 2 vectors together

        macro_rules! test_ranges{
            ($($arg: expr), +) => {
                {
                    $(
                        assert!(
                        retrieve_range_and_check(&storage_adapter, escrow_accounts.clone(), &received_receipt_vec, $arg)
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn remove_receipts_in_timestamp_range(pgpool: PgPool) {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ))
        .1;

        let storage_adapter =
            TapAgentContext::new(pgpool, ALLOCATION_ID_0, SENDER.1, escrow_accounts.clone());

        // Creating 10 receipts with timestamps 42 to 51
        let mut received_receipt_vec = Vec::new();
        for i in 0..10 {
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_0,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));

            // Adding irrelevant receipts to make sure they are not retrieved
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_IRRELEVANT,
                &SIGNER.0,
                i + 684,
                i + 42,
                (i + 124).into(),
            ));
            received_receipt_vec.push(create_received_receipt(
                &ALLOCATION_ID_0,
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
                            remove_range_and_check(&storage_adapter, escrow_accounts.clone(), &received_receipt_vec, $arg)
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
