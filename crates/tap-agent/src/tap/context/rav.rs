// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use bigdecimal::{
    num_bigint::{BigInt, ToBigInt},
    ToPrimitive,
};
use sqlx::types::{chrono, BigDecimal};
use tap_core::manager::adapters::{RavRead, RavStore};
use tap_graph::{ReceiptAggregateVoucher, SignedRav};
#[allow(deprecated)]
use thegraph_core::alloy::signers::Signature;
use thegraph_core::{
    alloy::{
        hex::ToHexExt,
        primitives::{Address, Bytes, FixedBytes},
    },
    CollectionId,
};

use super::{error::AdapterError, Horizon, Legacy, TapAgentContext};

/// Implements a [RavRead] for [tap_graph::ReceiptAggregateVoucher]
/// in case [super::NetworkVersion] is [Legacy]
///
/// This is important because RAVs for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl RavRead<ReceiptAggregateVoucher> for TapAgentContext<Legacy> {
    type AdapterError = AdapterError;

    async fn last_rav(&self) -> Result<Option<SignedRav>, Self::AdapterError> {
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
                Ok(Some(SignedRav {
                    message: rav,
                    signature,
                }))
            }
            None => Ok(None),
        }
    }
}

/// Implements a [RavStore] for [tap_graph::ReceiptAggregateVoucher]
/// in case [super::NetworkVersion] is [Legacy]
///
/// This is important because RAVs for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl RavStore<ReceiptAggregateVoucher> for TapAgentContext<Legacy> {
    type AdapterError = AdapterError;

    async fn update_last_rav(&self, rav: SignedRav) -> Result<(), Self::AdapterError> {
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

/// Implements a [RavRead] for [tap_graph::v2::ReceiptAggregateVoucher]
/// in case [super::NetworkVersion] is [Horizon]
///
/// This is important because RAVs for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl RavRead<tap_graph::v2::ReceiptAggregateVoucher> for TapAgentContext<Horizon> {
    type AdapterError = AdapterError;

    async fn last_rav(&self) -> Result<Option<tap_graph::v2::SignedRav>, Self::AdapterError> {
        // TODO add data service filter
        let row = sqlx::query!(
            r#"
                SELECT 
                    signature,
                    collection_id,
                    payer,
                    data_service,
                    service_provider,
                    timestamp_ns,
                    value_aggregate,
                    metadata
                FROM tap_horizon_ravs
                WHERE 
                    collection_id = $1 
                    AND payer = $2
                    AND service_provider = $3
            "#,
            CollectionId::from(self.allocation_id).encode_hex(),
            self.sender.encode_hex(),
            self.indexer_address.encode_hex()
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
                let collection_id =
                    FixedBytes::<32>::from_str(&row.collection_id).map_err(|e| {
                        AdapterError::RavRead {
                            error: format!(
                            "Error decoding collection_id while retrieving RAV from database: {}",
                            e
                        ),
                        }
                    })?;

                let payer = Address::from_str(&row.payer).map_err(|e| AdapterError::RavRead {
                    error: format!(
                        "Error decoding payer while retrieving receipt from database: {}",
                        e
                    ),
                })?;

                let data_service = Address::from_str(&row.data_service).map_err(|e| {
                    AdapterError::RavRead {
                        error: format!(
                            "Error decoding data_service while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;

                let service_provider = Address::from_str(&row.service_provider).map_err(|e| {
                    AdapterError::RavRead {
                        error: format!(
                            "Error decoding service_provider while retrieving receipt from database: {}",
                            e
                        ),
                    }
                })?;

                let metadata = Bytes::from(row.metadata);

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

                let rav = tap_graph::v2::ReceiptAggregateVoucher {
                    collectionId: collection_id,
                    timestampNs: timestamp_ns,
                    valueAggregate: value_aggregate,
                    dataService: data_service,
                    serviceProvider: service_provider,
                    payer,
                    metadata,
                };
                Ok(Some(tap_graph::v2::SignedRav {
                    message: rav,
                    signature,
                }))
            }
            None => Ok(None),
        }
    }
}

/// Implements a [RavStore] for [tap_graph::v2::ReceiptAggregateVoucher]
/// in case [super::NetworkVersion] is [Horizon]
///
/// This is important because RAVs for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl RavStore<tap_graph::v2::ReceiptAggregateVoucher> for TapAgentContext<Horizon> {
    type AdapterError = AdapterError;

    async fn update_last_rav(
        &self,
        rav: tap_graph::v2::SignedRav,
    ) -> Result<(), Self::AdapterError> {
        let signature_bytes: Vec<u8> = rav.signature.as_bytes().to_vec();

        let _fut = sqlx::query!(
            r#"
                INSERT INTO tap_horizon_ravs (
                    payer,
                    data_service,
                    service_provider,
                    metadata,
                    signature,
                    collection_id,
                    timestamp_ns,
                    value_aggregate,
                    created_at,
                    updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
                ON CONFLICT (payer, data_service, service_provider, collection_id)
                DO UPDATE SET
                    signature = $5,
                    timestamp_ns = $7,
                    value_aggregate = $8,
                    updated_at = $9,
                    metadata = $4
            "#,
            rav.message.payer.encode_hex(),
            rav.message.dataService.encode_hex(),
            rav.message.serviceProvider.encode_hex(),
            rav.message.metadata.as_ref(),
            signature_bytes,
            rav.message.collectionId.encode_hex(),
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
    use rstest::rstest;
    use sqlx::PgPool;
    use tap_core::signed_message::Eip712SignedMessage;
    use test_assets::TAP_SIGNER as SIGNER;
    use tokio::sync::watch;

    use super::*;
    use crate::{
        tap::context::NetworkVersion,
        test::{CreateRav, ALLOCATION_ID_0},
    };

    #[derive(Debug)]
    struct TestableRav<T: NetworkVersion>(Eip712SignedMessage<T::Rav>);

    impl<T: NetworkVersion> Eq for TestableRav<T> {}

    impl<T: NetworkVersion> PartialEq for TestableRav<T> {
        fn eq(&self, other: &Self) -> bool {
            self.0.message == other.0.message
                && self.0.signature.as_bytes() == other.0.signature.as_bytes()
        }
    }

    const TIMESTAMP_NS: u64 = u64::MAX - 10;
    const VALUE_AGGREGATE: u128 = u128::MAX;

    async fn legacy_adapter(pgpool: PgPool) -> TapAgentContext<Legacy> {
        TapAgentContext::builder()
            .pgpool(pgpool)
            .escrow_accounts(watch::channel(EscrowAccounts::default()).1)
            .build()
    }

    async fn horizon_adapter(pgpool: PgPool) -> TapAgentContext<Horizon> {
        TapAgentContext::builder()
            .pgpool(pgpool)
            .escrow_accounts(watch::channel(EscrowAccounts::default()).1)
            .build()
    }

    /// Insert a single receipt and retrieve it from the database using the adapter.
    /// The point here it to test the deserialization of large numbers.
    #[rstest]
    #[case(legacy_adapter(_pgpool.clone()))]
    #[case(horizon_adapter(_pgpool.clone()))]
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_and_retrieve_rav<T>(
        #[ignore] _pgpool: PgPool,
        #[case]
        #[future(awt)]
        context: TapAgentContext<T>,
    ) where
        T: CreateRav + std::fmt::Debug,
        TapAgentContext<T>: RavRead<T::Rav> + RavStore<T::Rav>,
    {
        // Insert a rav
        let mut new_rav = T::create_rav(
            Some(ALLOCATION_ID_0),
            None,
            SIGNER.0.clone(),
            TIMESTAMP_NS,
            VALUE_AGGREGATE,
        );
        context.update_last_rav(new_rav.clone()).await.unwrap();

        // Should trigger a retrieve_last_rav So eventually the last rav should be the one
        // we inserted
        let last_rav = context.last_rav().await.unwrap().unwrap();

        assert_eq!(TestableRav::<T>(new_rav.clone()), TestableRav(last_rav));

        // Update the RAV 3 times in quick succession
        for i in 0..3 {
            new_rav = T::create_rav(
                Some(ALLOCATION_ID_0),
                None,
                SIGNER.0.clone(),
                TIMESTAMP_NS + i,
                VALUE_AGGREGATE - (i as u128),
            );
            context.update_last_rav(new_rav.clone()).await.unwrap();
        }

        // Check that the last rav is the last one we inserted
        let last_rav = context.last_rav().await.unwrap();
        assert_eq!(TestableRav::<T>(new_rav), TestableRav(last_rav.unwrap()));
    }
}
