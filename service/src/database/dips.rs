// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use alloy::rlp::Decodable;
use anyhow::bail;
use axum::async_trait;
use build_info::chrono::Utc;

use indexer_dips::{SignedCancellationRequest, SignedIndexingAgreementVoucher};
use sqlx::PgPool;
use uuid::Uuid;

#[async_trait]
pub trait AgreementStore: Sync + Send {
    async fn get_by_id(&self, id: Uuid) -> anyhow::Result<Option<SignedIndexingAgreementVoucher>>;
    async fn create_agreement(
        &self,
        id: Uuid,
        agreement: SignedIndexingAgreementVoucher,
        protocol: String,
    ) -> anyhow::Result<()>;
    async fn cancel_agreement(
        &self,
        id: Uuid,
        signed_cancellation: SignedCancellationRequest,
    ) -> anyhow::Result<Uuid>;
}

#[derive(Default)]
pub struct InMemoryAgreementStore {
    pub data: tokio::sync::RwLock<HashMap<Uuid, SignedIndexingAgreementVoucher>>,
}

#[async_trait]
impl AgreementStore for InMemoryAgreementStore {
    async fn get_by_id(&self, id: Uuid) -> anyhow::Result<Option<SignedIndexingAgreementVoucher>> {
        Ok(self.data.try_read()?.get(&id).cloned())
    }
    async fn create_agreement(
        &self,
        id: Uuid,
        agreement: SignedIndexingAgreementVoucher,
        _protocol: String,
    ) -> anyhow::Result<()> {
        self.data.try_write()?.insert(id, agreement.clone());

        Ok(())
    }
    async fn cancel_agreement(
        &self,
        id: Uuid,
        _signed_cancellation: SignedCancellationRequest,
    ) -> anyhow::Result<Uuid> {
        self.data.try_write()?.remove(&id);

        Ok(id)
    }
}

#[derive(Debug)]
pub struct PsqlAgreementStore {
    pool: PgPool,
}

#[async_trait]
impl AgreementStore for PsqlAgreementStore {
    async fn get_by_id(&self, id: Uuid) -> anyhow::Result<Option<SignedIndexingAgreementVoucher>> {
        let item = sqlx::query!("SELECT * FROM dips_agreements WHERE id=$1", id,)
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
        protocol: String,
    ) -> anyhow::Result<()> {
        let bs = agreement.encode_vec();
        let now = Utc::now();

        sqlx::query!(
            "INSERT INTO dips_agreements VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,null,null)",
            id,
            agreement.signature.as_ref(),
            bs,
            protocol,
            agreement.voucher.service.as_slice(),
            agreement.voucher.payee.as_slice(),
            agreement.voucher.payer.as_slice(),
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
            "UPDATE dips_agreements SET updated_at=$1, cancelled_at=$1, signed_cancellation_payload=$2 WHERE id=$3",
            now,
            bs,
            id,
        )
        .execute(&self.pool)
        .await?;

        Ok(id)
    }
}
