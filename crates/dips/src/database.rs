// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::any::Any;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{store::RcaStore, DipsError};

/// PostgreSQL implementation of RcaStore for RecurringCollectionAgreement.
#[derive(Debug)]
pub struct PsqlRcaStore {
    pub pool: PgPool,
}

#[async_trait]
impl RcaStore for PsqlRcaStore {
    async fn store_rca(
        &self,
        agreement_id: Uuid,
        signed_rca: Vec<u8>,
        version: u64,
    ) -> Result<(), DipsError> {
        sqlx::query(
            "INSERT INTO pending_rca_proposals (id, signed_payload, version, status, created_at, updated_at)
             VALUES ($1, $2, $3, 'pending', NOW(), NOW())"
        )
        .bind(agreement_id)
        .bind(signed_rca)
        .bind(version as i16)
        .execute(&self.pool)
        .await
        .map_err(|e| DipsError::UnknownError(e.into()))?;

        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
