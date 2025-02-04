// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use anyhow::bail;
use axum::async_trait;
use build_info::chrono::Utc;
use indexer_dips::{
    store::AgreementStore, SignedCancellationRequest, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};
use sqlx::{types::BigDecimal, PgPool};
use thegraph_core::alloy::{core::primitives::U256 as uint256, rlp::Decodable};
use uuid::Uuid;

#[derive(Debug)]
pub struct PsqlAgreementStore {
    pub pool: PgPool,
}

// Utility to convert alloy::alloy_primitives::Uint<256, 4> into sqlx::BigDecimal
fn uint256_to_bigdecimal(uint256: &uint256) -> BigDecimal {
    BigDecimal::from_str(&uint256.to_string()).unwrap()
}

#[async_trait]
impl AgreementStore for PsqlAgreementStore {
    async fn get_by_id(&self, id: Uuid) -> anyhow::Result<Option<SignedIndexingAgreementVoucher>> {
        let item = sqlx::query!("SELECT * FROM indexing_agreements WHERE id=$1", id,)
            .fetch_one(&self.pool)
            .await;

        let item = match item {
            Ok(item) => item,
            Err(sqlx::Error::RowNotFound) => return Ok(None),
            Err(err) => bail!(err),
        };

        let signed = SignedIndexingAgreementVoucher::decode(&mut item.signed_payload.as_ref())?;

        Ok(Some(signed))
    }
    async fn create_agreement(
        &self,
        id: Uuid,
        agreement: SignedIndexingAgreementVoucher,
        metadata: SubgraphIndexingVoucherMetadata,
    ) -> anyhow::Result<()> {
        let bs = agreement.encode_vec();
        let now = Utc::now();

        let price_per_block = uint256_to_bigdecimal(&metadata.pricePerBlock);
        let price_per_entity = uint256_to_bigdecimal(&metadata.pricePerEntity);
        sqlx::query!(
            "INSERT INTO indexing_agreements VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,null,null,null)",
            id,
            agreement.signature.as_ref(),
            bs,
            metadata.protocolNetwork,
            metadata.chainId,
            price_per_block,
            price_per_entity,
            metadata.subgraphDeploymentId,
            format!("0x{}", agreement.voucher.service),
            format!("0x{}", agreement.voucher.recipient),
            format!("0x{}", agreement.voucher.payer),
            now,
            now,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
    async fn cancel_agreement(
        &self,
        id: Uuid,
        signed_cancellation: SignedCancellationRequest,
    ) -> anyhow::Result<Uuid> {
        let bs = signed_cancellation.encode_vec();
        let now = Utc::now();

        sqlx::query!(
            "UPDATE indexing_agreements SET updated_at=$1, cancelled_at=$1, signed_cancellation_payload=$2 WHERE id=$3",
            now,
            bs,
            id,
        )
        .execute(&self.pool)
        .await?;

        Ok(id)
    }
}
