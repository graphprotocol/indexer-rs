// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use bigdecimal::{
    num_bigint::{BigInt, ToBigInt},
    ToPrimitive,
};
use sqlx::types::{chrono, BigDecimal};
use tap_core::{
    manager::adapters::{RAVRead, RAVStore},
    rav::{ReceiptAggregateVoucher, SignedRAV},
};
#[allow(deprecated)]
use thegraph_core::alloy::signers::Signature;
use thegraph_core::alloy::{hex::ToHexExt, primitives::Address};

use super::{error::AdapterError, TapAgentContext};

#[async_trait::async_trait]
impl RAVRead for TapAgentContext {
    type AdapterError = AdapterError;

    async fn last_rav(&self) -> Result<Option<SignedRAV>, Self::AdapterError> {
        let row = sqlx::query!(
            r#"
                SELECT signature, allocation_id, timestamp_ns, value_aggregate
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            self.allocation_id.encode_hex(),
            self.sender.encode_hex()
        )
        .fetch_optional(&self.pgpool)
        .await
        .map_err(|e| AdapterError::RavRead {
            error: e.to_string(),
        })?;

        match row {
            Some(row) => {
                #[allow(deprecated)]
                let signature: Signature =
                    row.signature
                        .as_slice()
                        .try_into()
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                                "Error decoding signature while retrieving RAV from database: {}",
                                e
                            ),
                        })?;
                let allocation_id =
                    Address::from_str(&row.allocation_id).map_err(|e| AdapterError::RavRead {
                        error: format!(
                            "Error decoding allocation_id while retrieving RAV from database: {}",
                            e
                        ),
                    })?;
                let timestamp_ns = row.timestamp_ns.to_u64().ok_or(AdapterError::RavRead {
                    error: "Error decoding timestamp_ns while retrieving RAV from database"
                        .to_string(),
                })?;
                let value_aggregate = row
                    .value_aggregate
                    // Beware, BigDecimal::to_u128() actually uses to_u64() under the hood.
                    // So we're converting to BigInt to get a proper implementation of to_u128().
                    .to_bigint()
                    .and_then(|v| v.to_u128())
                    .ok_or(AdapterError::RavRead {
                        error: "Error decoding value_aggregate while retrieving RAV from database"
                            .to_string(),
                    })?;

                let rav = ReceiptAggregateVoucher {
                    allocationId: allocation_id,
                    timestampNs: timestamp_ns,
                    valueAggregate: value_aggregate,
                };
                Ok(Some(SignedRAV {
                    message: rav,
                    signature,
                }))
            }
            None => Ok(None),
        }
    }
}

#[async_trait::async_trait]
impl RAVStore for TapAgentContext {
    type AdapterError = AdapterError;

    async fn update_last_rav(&self, rav: SignedRAV) -> Result<(), Self::AdapterError> {
        let signature_bytes: Vec<u8> = rav.signature.as_bytes().to_vec();

        let _fut = sqlx::query!(
            r#"
                INSERT INTO scalar_tap_ravs (
                    sender_address,
                    signature,
                    allocation_id,
                    timestamp_ns,
                    value_aggregate,
                    created_at,
                    updated_at

                )
                VALUES ($1, $2, $3, $4, $5, $6, $6)
                ON CONFLICT (allocation_id, sender_address)
                DO UPDATE SET
                    signature = $2,
                    timestamp_ns = $4,
                    value_aggregate = $5,
                    updated_at = $6
            "#,
            self.sender.encode_hex(),
            signature_bytes,
            self.allocation_id.encode_hex(),
            BigDecimal::from(rav.message.timestampNs),
            BigDecimal::from(BigInt::from(rav.message.valueAggregate)),
            chrono::Utc::now()
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| AdapterError::RavStore {
            error: e.to_string(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use indexer_monitor::EscrowAccounts;
    use sqlx::PgPool;
    use test_assets::{TAP_SENDER as SENDER, TAP_SIGNER as SIGNER};
    use tokio::sync::watch;

    use super::*;
    use crate::test::{create_rav, ALLOCATION_ID_0};

    #[derive(Debug)]
    struct TestableRav(SignedRAV);

    impl Eq for TestableRav {}

    impl PartialEq for TestableRav {
        fn eq(&self, other: &Self) -> bool {
            self.0.message == other.0.message
                && self.0.signature.as_bytes() == other.0.signature.as_bytes()
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_and_retrieve_rav(pool: PgPool) {
        let timestamp_ns = u64::MAX - 10;
        let value_aggregate = u128::MAX;
        let context = TapAgentContext::new(
            pool.clone(),
            ALLOCATION_ID_0,
            SENDER.1,
            watch::channel(EscrowAccounts::default()).1,
        );

        // Insert a rav
        let mut new_rav = create_rav(
            ALLOCATION_ID_0,
            SIGNER.0.clone(),
            timestamp_ns,
            value_aggregate,
        );
        context.update_last_rav(new_rav.clone()).await.unwrap();

        // Should trigger a retrieve_last_rav So eventually the last rav should be the one
        // we inserted
        let last_rav = context.last_rav().await.unwrap().unwrap();

        assert_eq!(TestableRav(new_rav.clone()), TestableRav(last_rav));

        // Update the RAV 3 times in quick succession
        for i in 0..3 {
            new_rav = create_rav(
                ALLOCATION_ID_0,
                SIGNER.0.clone(),
                timestamp_ns + i,
                value_aggregate - (i as u128),
            );
            context.update_last_rav(new_rav.clone()).await.unwrap();
        }

        // Check that the last rav is the last one we inserted
        let last_rav = context.last_rav().await.unwrap();
        assert_eq!(TestableRav(new_rav), TestableRav(last_rav.unwrap()));
    }
}
