// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use async_trait::async_trait;
use build_info::chrono::{DateTime, Utc};
use sqlx::{types::BigDecimal, PgPool, Row};
use thegraph_core::alloy::{core::primitives::U256 as uint256, hex::ToHexExt, sol_types::SolType};
use uuid::Uuid;

use crate::{
    store::{AgreementStore, StoredIndexingAgreement},
    DipsError, SignedCancellationRequest, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};

#[derive(Debug)]
pub struct PsqlAgreementStore {
    pub pool: PgPool,
}

fn uint256_to_bigdecimal(value: &uint256, field: &str) -> Result<BigDecimal, DipsError> {
    BigDecimal::from_str(&value.to_string())
        .map_err(|e| DipsError::InvalidVoucher(format!("{field}: {e}")))
}

#[async_trait]
impl AgreementStore for PsqlAgreementStore {
    async fn get_by_id(&self, id: Uuid) -> Result<Option<StoredIndexingAgreement>, DipsError> {
        let item = sqlx::query("SELECT * FROM indexing_agreements WHERE id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await;

        let item = match item {
            Ok(item) => item,
            Err(sqlx::Error::RowNotFound) => return Ok(None),
            Err(err) => return Err(DipsError::UnknownError(err.into())),
        };

        let signed_payload: Vec<u8> = item
            .try_get("signed_payload")
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        let signed = SignedIndexingAgreementVoucher::abi_decode(signed_payload.as_ref())
            .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;
        let metadata =
            SubgraphIndexingVoucherMetadata::abi_decode(signed.voucher.metadata.as_ref())
                .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;
        let cancelled_at: Option<DateTime<Utc>> = item
            .try_get("cancelled_at")
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        let cancelled = cancelled_at.is_some();
        let current_allocation_id: Option<String> = item
            .try_get("current_allocation_id")
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        let last_allocation_id: Option<String> = item
            .try_get("last_allocation_id")
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        let last_payment_collected_at: Option<DateTime<Utc>> = item
            .try_get("last_payment_collected_at")
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        Ok(Some(StoredIndexingAgreement {
            voucher: signed,
            metadata,
            cancelled,
            current_allocation_id,
            last_allocation_id,
            last_payment_collected_at,
        }))
    }
    async fn create_agreement(
        &self,
        agreement: SignedIndexingAgreementVoucher,
        metadata: SubgraphIndexingVoucherMetadata,
    ) -> Result<(), DipsError> {
        let id = Uuid::from_bytes(agreement.voucher.agreement_id.into());
        let bs = agreement.encode_vec();
        let now = Utc::now();
        let deadline_i64: i64 = agreement
            .voucher
            .deadline
            .try_into()
            .map_err(|_| DipsError::InvalidVoucher("deadline".to_string()))?;
        let deadline = DateTime::from_timestamp(deadline_i64, 0)
            .ok_or(DipsError::InvalidVoucher("deadline".to_string()))?;
        let base_price_per_epoch =
            uint256_to_bigdecimal(&metadata.basePricePerEpoch, "basePricePerEpoch")?;
        let price_per_entity = uint256_to_bigdecimal(&metadata.pricePerEntity, "pricePerEntity")?;
        let duration_epochs: i64 = agreement.voucher.durationEpochs.into();
        let max_initial_amount =
            uint256_to_bigdecimal(&agreement.voucher.maxInitialAmount, "maxInitialAmount")?;
        let max_ongoing_amount_per_epoch = uint256_to_bigdecimal(
            &agreement.voucher.maxOngoingAmountPerEpoch,
            "maxOngoingAmountPerEpoch",
        )?;
        let min_epochs_per_collection: i64 = agreement.voucher.minEpochsPerCollection.into();
        let max_epochs_per_collection: i64 = agreement.voucher.maxEpochsPerCollection.into();
        sqlx::query(
            "INSERT INTO indexing_agreements VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,null,null,null,null,null)",
        )
        .bind(id)
        .bind(agreement.signature.as_ref())
        .bind(bs)
        .bind(metadata.protocolNetwork)
        .bind(metadata.chainId)
        .bind(base_price_per_epoch)
        .bind(price_per_entity)
        .bind(metadata.subgraphDeploymentId)
        .bind(agreement.voucher.service.encode_hex())
        .bind(agreement.voucher.recipient.encode_hex())
        .bind(agreement.voucher.payer.encode_hex())
        .bind(deadline)
        .bind(duration_epochs)
        .bind(max_initial_amount)
        .bind(max_ongoing_amount_per_epoch)
        .bind(min_epochs_per_collection)
        .bind(max_epochs_per_collection)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| DipsError::UnknownError(e.into()))?;

        Ok(())
    }
    async fn cancel_agreement(
        &self,
        signed_cancellation: SignedCancellationRequest,
    ) -> Result<Uuid, DipsError> {
        let id = Uuid::from_bytes(signed_cancellation.request.agreement_id.into());
        let bs = signed_cancellation.encode_vec();
        let now = Utc::now();

        sqlx::query(
            "UPDATE indexing_agreements SET updated_at=$1, cancelled_at=$1, signed_cancellation_payload=$2 WHERE id=$3",
        )
        .bind(now)
        .bind(bs)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|_| DipsError::AgreementNotFound)?;

        Ok(id)
    }
}

#[cfg(test)]
pub(crate) mod test {
    use std::sync::Arc;

    use build_info::chrono::Duration;
    use sqlx::Row;
    use thegraph_core::alloy::{
        primitives::{ruint::aliases::U256, Address},
        sol_types::SolValue,
    };
    use uuid::Uuid;

    use super::*;
    use crate::{CancellationRequest, IndexingAgreementVoucher};

    #[tokio::test]
    async fn test_store_agreement() {
        let test_db = test_assets::setup_shared_test_db().await;
        let store = Arc::new(PsqlAgreementStore { pool: test_db.pool });
        let id = Uuid::now_v7();

        // Create metadata first
        let metadata = SubgraphIndexingVoucherMetadata {
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            basePricePerEpoch: U256::from(5000),
            pricePerEntity: U256::from(10),
            subgraphDeploymentId: "Qm123".to_string(),
        };

        // Create agreement with encoded metadata
        let agreement = SignedIndexingAgreementVoucher {
            signature: vec![1, 2, 3].into(),
            voucher: IndexingAgreementVoucher {
                agreement_id: id.as_bytes().into(),
                deadline: (Utc::now() + Duration::days(30)).timestamp() as u64,
                payer: Address::from_str("1234567890123456789012345678901234567890").unwrap(),
                recipient: Address::from_str("2345678901234567890123456789012345678901").unwrap(),
                service: Address::from_str("3456789012345678901234567890123456789012").unwrap(),
                durationEpochs: 30, // 30 epochs duration
                maxInitialAmount: U256::from(1000),
                maxOngoingAmountPerEpoch: U256::from(100),
                maxEpochsPerCollection: 5,
                minEpochsPerCollection: 1,
                metadata: metadata.abi_encode().into(), // Convert Vec<u8> to Bytes
            },
        };

        // Store agreement
        store
            .create_agreement(agreement.clone(), metadata)
            .await
            .unwrap();

        // Verify stored agreement
        let row = sqlx::query("SELECT * FROM indexing_agreements WHERE id = $1")
            .bind(id)
            .fetch_one(&store.pool)
            .await
            .unwrap();

        let row_id: Uuid = row.try_get("id").unwrap();
        let signature: Vec<u8> = row.try_get("signature").unwrap();
        let protocol_network: String = row.try_get("protocol_network").unwrap();
        let chain_id: String = row.try_get("chain_id").unwrap();
        let subgraph_deployment_id: String = row.try_get("subgraph_deployment_id").unwrap();

        assert_eq!(row_id, id);
        assert_eq!(signature, agreement.signature);
        assert_eq!(protocol_network, "eip155:42161");
        assert_eq!(chain_id, "eip155:1");
        assert_eq!(subgraph_deployment_id, "Qm123");
    }

    #[tokio::test]
    async fn test_get_agreement_by_id() {
        let test_db = test_assets::setup_shared_test_db().await;
        let store = Arc::new(PsqlAgreementStore { pool: test_db.pool });
        let id = Uuid::parse_str("a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d9").unwrap();

        // Create metadata first
        let metadata = SubgraphIndexingVoucherMetadata {
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            basePricePerEpoch: U256::from(5000),
            pricePerEntity: U256::from(10),
            subgraphDeploymentId: "Qm123".to_string(),
        };

        // Create agreement with encoded metadata
        let agreement = SignedIndexingAgreementVoucher {
            signature: vec![1, 2, 3].into(),
            voucher: IndexingAgreementVoucher {
                agreement_id: id.as_bytes().into(),
                deadline: (Utc::now() + Duration::days(30)).timestamp() as u64,
                payer: Address::from_str("1234567890123456789012345678901234567890").unwrap(),
                recipient: Address::from_str("2345678901234567890123456789012345678901").unwrap(),
                service: Address::from_str("3456789012345678901234567890123456789012").unwrap(),
                durationEpochs: 30,
                maxInitialAmount: U256::from(1000),
                maxOngoingAmountPerEpoch: U256::from(100),
                maxEpochsPerCollection: 5,
                minEpochsPerCollection: 1,
                metadata: metadata.abi_encode().into(),
            },
        };

        // Store agreement
        store
            .create_agreement(agreement.clone(), metadata.clone())
            .await
            .unwrap();

        // Retrieve agreement
        let stored_agreement = store.get_by_id(id).await.unwrap().unwrap();

        let retrieved_voucher = &stored_agreement.voucher;
        let retrieved_metadata = stored_agreement.metadata;

        // Verify retrieved agreement matches original
        assert_eq!(retrieved_voucher.signature, agreement.signature);
        assert_eq!(
            retrieved_voucher.voucher.durationEpochs,
            agreement.voucher.durationEpochs
        );
        assert_eq!(retrieved_metadata.protocolNetwork, metadata.protocolNetwork);
        assert_eq!(retrieved_metadata.chainId, metadata.chainId);
        assert_eq!(
            retrieved_metadata.subgraphDeploymentId,
            metadata.subgraphDeploymentId
        );
        assert_eq!(retrieved_voucher.voucher.payer, agreement.voucher.payer);
        assert_eq!(
            retrieved_voucher.voucher.recipient,
            agreement.voucher.recipient
        );
        assert_eq!(retrieved_voucher.voucher.service, agreement.voucher.service);
        assert_eq!(
            retrieved_voucher.voucher.maxInitialAmount,
            agreement.voucher.maxInitialAmount
        );
        assert_eq!(
            retrieved_voucher.voucher.maxOngoingAmountPerEpoch,
            agreement.voucher.maxOngoingAmountPerEpoch
        );
        assert_eq!(
            retrieved_voucher.voucher.maxEpochsPerCollection,
            agreement.voucher.maxEpochsPerCollection
        );
        assert_eq!(
            retrieved_voucher.voucher.minEpochsPerCollection,
            agreement.voucher.minEpochsPerCollection
        );
        assert!(!stored_agreement.cancelled);
    }

    #[tokio::test]
    async fn test_cancel_agreement() {
        let test_db = test_assets::setup_shared_test_db().await;
        let store = Arc::new(PsqlAgreementStore { pool: test_db.pool });
        let id = Uuid::parse_str("a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7e9").unwrap();

        // Create metadata first
        let metadata = SubgraphIndexingVoucherMetadata {
            protocolNetwork: "eip155:42161".to_string(),
            chainId: "eip155:1".to_string(),
            basePricePerEpoch: U256::from(5000),
            pricePerEntity: U256::from(10),
            subgraphDeploymentId: "Qm123".to_string(),
        };

        // Create agreement with encoded metadata
        let agreement = SignedIndexingAgreementVoucher {
            signature: vec![1, 2, 3].into(),
            voucher: IndexingAgreementVoucher {
                agreement_id: id.as_bytes().into(),
                deadline: (Utc::now() + Duration::days(30)).timestamp() as u64,
                payer: Address::from_str("1234567890123456789012345678901234567890").unwrap(),
                recipient: Address::from_str("2345678901234567890123456789012345678901").unwrap(),
                service: Address::from_str("3456789012345678901234567890123456789012").unwrap(),
                durationEpochs: 30,
                maxInitialAmount: U256::from(1000),
                maxOngoingAmountPerEpoch: U256::from(100),
                maxEpochsPerCollection: 5,
                minEpochsPerCollection: 1,
                metadata: metadata.abi_encode().into(),
            },
        };

        // Store agreement
        store
            .create_agreement(agreement.clone(), metadata)
            .await
            .unwrap();

        // Cancel agreement
        let cancellation = SignedCancellationRequest {
            signature: vec![1, 2, 3].into(),
            request: CancellationRequest {
                agreement_id: id.as_bytes().into(),
            },
        };
        store.cancel_agreement(cancellation.clone()).await.unwrap();

        // Verify stored agreement
        let row = sqlx::query("SELECT * FROM indexing_agreements WHERE id = $1")
            .bind(id)
            .fetch_one(&store.pool)
            .await
            .unwrap();

        let cancelled_at: Option<DateTime<Utc>> = row.try_get("cancelled_at").unwrap();
        let signed_cancellation_payload: Option<Vec<u8>> =
            row.try_get("signed_cancellation_payload").unwrap();
        assert!(cancelled_at.is_some());
        assert_eq!(signed_cancellation_payload, Some(cancellation.encode_vec()));
    }
}
