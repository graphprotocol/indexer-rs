// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use bigdecimal::{
    num_bigint::{BigInt, ToBigInt},
    ToPrimitive,
};
use sqlx::{
    types::{chrono, BigDecimal},
    Row,
};
use tap_core::manager::adapters::{RavRead, RavStore};
#[allow(deprecated)]
use thegraph_core::alloy::signers::Signature;
use thegraph_core::{
    alloy::{
        hex::ToHexExt,
        primitives::{Address, Bytes, FixedBytes},
    },
    CollectionId,
};

use super::{error::AdapterError, Horizon, TapAgentContext};

/// Implements a [RavRead] for [tap_graph::v2::ReceiptAggregateVoucher]
/// in case [super::NetworkVersion] is [Horizon]
///
/// This is important because RAVs for each network version
/// are stored in a different database table
#[async_trait::async_trait]
impl RavRead<tap_graph::v2::ReceiptAggregateVoucher> for TapAgentContext<Horizon> {
    type AdapterError = AdapterError;

    async fn last_rav(&self) -> Result<Option<tap_graph::v2::SignedRav>, Self::AdapterError> {
        let row = sqlx::query(
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
                    AND data_service = $3
                    AND service_provider = $4
            "#,
        )
        .bind(CollectionId::from(self.allocation_id).encode_hex())
        .bind(self.sender.encode_hex())
        // For Horizon (V2): data_service is the SubgraphService address, service_provider is the indexer
        .bind(
            self.subgraph_service_address()
                .ok_or_else(|| AdapterError::RavRead {
                    error: "SubgraphService address not available - check TapMode configuration"
                        .to_string(),
                })?
                .encode_hex(),
        )
        .bind(self.indexer_address.encode_hex())
        .fetch_optional(&self.pgpool)
        .await
        .map_err(|e| AdapterError::RavRead {
            error: e.to_string(),
        })?;

        match row {
            Some(row) => {
                #[allow(deprecated)]
                let signature_bytes: Vec<u8> =
                    row.try_get("signature")
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                                "Error decoding signature while retrieving RAV from database: {e}"
                            ),
                        })?;
                #[allow(deprecated)]
                let signature: Signature =
                    signature_bytes
                        .as_slice()
                        .try_into()
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                                "Error decoding signature while retrieving RAV from database: {e}"
                            ),
                        })?;
                let collection_id: String =
                    row.try_get("collection_id")
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                            "Error decoding collection_id while retrieving RAV from database: {e}"
                        ),
                        })?;
                let collection_id = FixedBytes::<32>::from_str(&collection_id).map_err(|e| {
                    AdapterError::RavRead {
                        error: format!(
                            "Error decoding collection_id while retrieving RAV from database: {e}"
                        ),
                    }
                })?;

                let payer: String = row.try_get("payer").map_err(|e| AdapterError::RavRead {
                    error: format!(
                        "Error decoding payer while retrieving receipt from database: {e}"
                    ),
                })?;
                let payer = Address::from_str(&payer).map_err(|e| AdapterError::RavRead {
                    error: format!(
                        "Error decoding payer while retrieving receipt from database: {e}"
                    ),
                })?;

                let data_service: String =
                    row.try_get("data_service").map_err(|e| AdapterError::RavRead {
                        error: format!(
                            "Error decoding data_service while retrieving receipt from database: {e}"
                        ),
                    })?;
                let data_service = Address::from_str(&data_service).map_err(|e| {
                    AdapterError::RavRead {
                        error: format!(
                            "Error decoding data_service while retrieving receipt from database: {e}"
                        ),
                    }
                })?;

                let service_provider: String =
                    row.try_get("service_provider").map_err(|e| AdapterError::RavRead {
                        error: format!(
                            "Error decoding service_provider while retrieving receipt from database: {e}"
                        ),
                    })?;
                let service_provider = Address::from_str(&service_provider).map_err(|e| {
                    AdapterError::RavRead {
                        error: format!(
                            "Error decoding service_provider while retrieving receipt from database: {e}"
                        ),
                    }
                })?;

                let metadata: Vec<u8> =
                    row.try_get("metadata").map_err(|e| AdapterError::RavRead {
                        error: format!(
                            "Error decoding metadata while retrieving RAV from database: {e}"
                        ),
                    })?;
                let metadata = Bytes::from(metadata);

                let timestamp_ns: BigDecimal =
                    row.try_get("timestamp_ns")
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                            "Error decoding timestamp_ns while retrieving RAV from database: {e}"
                        ),
                        })?;
                let timestamp_ns = timestamp_ns.to_u64().ok_or(AdapterError::RavRead {
                    error: "Error decoding timestamp_ns while retrieving RAV from database"
                        .to_string(),
                })?;
                let value_aggregate: BigDecimal =
                    row.try_get("value_aggregate")
                        .map_err(|e| AdapterError::RavRead {
                            error: format!(
                            "Error decoding value_aggregate while retrieving RAV from database: {e}"
                        ),
                        })?;
                let value_aggregate = value_aggregate
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

        let _fut = sqlx::query(
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
        )
        .bind(rav.message.payer.encode_hex())
        .bind(rav.message.dataService.encode_hex())
        .bind(rav.message.serviceProvider.encode_hex())
        .bind(rav.message.metadata.as_ref())
        .bind(signature_bytes)
        .bind(rav.message.collectionId.encode_hex())
        .bind(BigDecimal::from(rav.message.timestampNs))
        .bind(BigDecimal::from(BigInt::from(rav.message.valueAggregate)))
        .bind(chrono::Utc::now())
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
    use tap_core::signed_message::Eip712SignedMessage;
    use test_assets::TAP_SIGNER as SIGNER;
    use tokio::sync::watch;

    use super::*;
    use crate::{
        tap::context::NetworkVersion,
        test::{CreateRav, ALLOCATION_ID_0, SUBGRAPH_SERVICE_ADDRESS},
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

    struct TestContextWithContainer<T> {
        context: TapAgentContext<T>,
        _test_db: test_assets::TestDatabase,
    }

    impl<T> std::ops::Deref for TestContextWithContainer<T> {
        type Target = TapAgentContext<T>;
        fn deref(&self) -> &Self::Target {
            &self.context
        }
    }

    async fn horizon_adapter_with_testcontainers() -> TestContextWithContainer<Horizon> {
        let test_db = test_assets::setup_shared_test_db().await;
        let context = TapAgentContext::builder()
            .pgpool(test_db.pool.clone())
            .escrow_accounts(watch::channel(EscrowAccounts::default()).1)
            .subgraph_service_address(SUBGRAPH_SERVICE_ADDRESS)
            .build();
        TestContextWithContainer {
            context,
            _test_db: test_db,
        }
    }

    /// Insert a single receipt and retrieve it from the database using the adapter.
    /// The point here it to test the deserialization of large numbers.
    #[rstest]
    #[case(horizon_adapter_with_testcontainers())]
    #[tokio::test]
    async fn update_and_retrieve_rav<T>(
        #[case]
        #[future(awt)]
        context: TestContextWithContainer<T>,
    ) where
        T: CreateRav + std::fmt::Debug,
        TapAgentContext<T>: RavRead<T::Rav> + RavStore<T::Rav>,
    {
        // Insert a rav
        let mut new_rav = T::create_rav(
            ALLOCATION_ID_0,
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
                ALLOCATION_ID_0,
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
