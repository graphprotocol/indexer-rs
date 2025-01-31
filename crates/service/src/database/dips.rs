// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use axum::async_trait;
use build_info::chrono::{DateTime, Utc};
use indexer_dips::{
    store::AgreementStore, DipsError, SignedCancellationRequest, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};
use sqlx::{types::BigDecimal, PgPool};
use thegraph_core::alloy::{core::primitives::U256 as uint256, hex::ToHexExt, sol_types::SolType};
use uuid::Uuid;

#[derive(Debug)]
pub struct PsqlAgreementStore {
    pub pool: PgPool,
}

fn uint256_to_bigdecimal(value: &uint256, field: &str) -> Result<BigDecimal, DipsError> {
    BigDecimal::from_str(&value.to_string())
        .map_err(|e| DipsError::InvalidVoucher(format!("{}: {}", field, e)))
}

#[async_trait]
impl AgreementStore for PsqlAgreementStore {
    async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<(SignedIndexingAgreementVoucher, bool)>, DipsError> {
        let item = sqlx::query!("SELECT * FROM indexing_agreements WHERE id=$1", id,)
            .fetch_one(&self.pool)
            .await;

        let item = match item {
            Ok(item) => item,
            Err(sqlx::Error::RowNotFound) => return Ok(None),
            Err(err) => return Err(DipsError::UnknownError(err.into())),
        };

        let signed = SignedIndexingAgreementVoucher::abi_decode(item.signed_payload.as_ref(), true)
            .map_err(|e| DipsError::AbiDecoding(e.to_string()))?;
        let cancelled = item.cancelled_at.is_some();
        Ok(Some((signed, cancelled)))
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
        sqlx::query!(
            "INSERT INTO indexing_agreements VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,null,null,null)",
            id,
            agreement.signature.as_ref(),
            bs,
            metadata.protocolNetwork,
            metadata.chainId,
            base_price_per_epoch,
            price_per_entity,
            metadata.subgraphDeploymentId,
            agreement.voucher.service.encode_hex(),
            agreement.voucher.recipient.encode_hex(),
            agreement.voucher.payer.encode_hex(),
            deadline,
            duration_epochs,
            max_initial_amount,
            max_ongoing_amount_per_epoch,
            min_epochs_per_collection,
            max_epochs_per_collection,
            now,
            now
        )
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

        sqlx::query!(
            "UPDATE indexing_agreements SET updated_at=$1, cancelled_at=$1, signed_cancellation_payload=$2 WHERE id=$3",
            now,
            bs,
            id,
        )
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
    use indexer_dips::{CancellationRequest, IndexingAgreementVoucher};
    use sqlx::PgPool;
    use thegraph_core::alloy::{
        primitives::{ruint::aliases::U256, Address},
        sol_types::{SolType, SolValue},
    };
    use uuid::Uuid;

    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_store_agreement(pool: PgPool) {
        let store = Arc::new(PsqlAgreementStore { pool });
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
        let row = sqlx::query!("SELECT * FROM indexing_agreements WHERE id = $1", id)
            .fetch_one(&store.pool)
            .await
            .unwrap();

        assert_eq!(row.id, id);
        assert_eq!(row.signature, agreement.signature);
        assert_eq!(row.protocol_network, "eip155:42161");
        assert_eq!(row.chain_id, "eip155:1");
        assert_eq!(row.subgraph_deployment_id, "Qm123");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_get_agreement_by_id(pool: PgPool) {
        let store = Arc::new(PsqlAgreementStore { pool });
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
        let (retrieved_signed_voucher, cancelled) = store.get_by_id(id).await.unwrap().unwrap();

        let retrieved_voucher = &retrieved_signed_voucher.voucher;
        let retrieved_metadata =
            <indexer_dips::SubgraphIndexingVoucherMetadata as SolType>::abi_decode(
                retrieved_voucher.metadata.as_ref(),
                true,
            )
            .unwrap();
        // Verify retrieved agreement matches original
        assert_eq!(retrieved_signed_voucher.signature, agreement.signature);
        assert_eq!(
            retrieved_voucher.durationEpochs,
            agreement.voucher.durationEpochs
        );
        assert_eq!(retrieved_metadata.protocolNetwork, metadata.protocolNetwork);
        assert_eq!(retrieved_metadata.chainId, metadata.chainId);
        assert_eq!(
            retrieved_metadata.subgraphDeploymentId,
            metadata.subgraphDeploymentId
        );
        assert_eq!(retrieved_voucher.payer, agreement.voucher.payer);
        assert_eq!(retrieved_voucher.recipient, agreement.voucher.recipient);
        assert_eq!(retrieved_voucher.service, agreement.voucher.service);
        assert_eq!(
            retrieved_voucher.maxInitialAmount,
            agreement.voucher.maxInitialAmount
        );
        assert_eq!(
            retrieved_voucher.maxOngoingAmountPerEpoch,
            agreement.voucher.maxOngoingAmountPerEpoch
        );
        assert_eq!(
            retrieved_voucher.maxEpochsPerCollection,
            agreement.voucher.maxEpochsPerCollection
        );
        assert_eq!(
            retrieved_voucher.minEpochsPerCollection,
            agreement.voucher.minEpochsPerCollection
        );
        assert!(!cancelled);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_cancel_agreement(pool: PgPool) {
        let store = Arc::new(PsqlAgreementStore { pool });
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
        let row = sqlx::query!("SELECT * FROM indexing_agreements WHERE id = $1", id)
            .fetch_one(&store.pool)
            .await
            .unwrap();

        assert!(row.cancelled_at.is_some());
        assert_eq!(
            row.signed_cancellation_payload,
            Some(cancellation.encode_vec())
        );
    }
}
