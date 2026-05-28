// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! PostgreSQL implementation of [`RcaStore`](crate::store::RcaStore).
//!
//! This module provides [`PsqlRcaStore`], which persists validated RCA proposals
//! to the `pending_rca_proposals` table. The indexer-agent queries this table
//! directly to find pending proposals and decide on-chain acceptance.
//!
//! # Shared Database
//!
//! indexer-rs (Rust) and indexer-agent (TypeScript) share the same PostgreSQL
//! database. This module only writes; the agent reads and updates status:
//!
//! ```text
//! indexer-rs ──INSERT──> pending_rca_proposals <──SELECT/UPDATE── indexer-agent
//! ```
//!
//! # Status Lifecycle
//!
//! 1. indexer-rs inserts with status = "pending"
//! 2. indexer-agent queries pending proposals
//! 3. Agent validates allocation availability, accepts on-chain
//! 4. Agent updates status to "accepted" or "rejected"
//!
//! # Idempotency
//!
//! The `store_rca` operation is idempotent: inserting the same agreement ID twice
//! succeeds both times. This handles retry scenarios where Dipper re-sends an RCA
//! after a timeout (network partition, crash after INSERT but before response, etc.).
//!
//! Without idempotency, the retry would fail with a duplicate key error, causing
//! Dipper to mark the agreement as failed even though it was successfully stored.

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
        // ON CONFLICT DO NOTHING makes this idempotent: retries with the same
        // agreement_id succeed without error, enabling safe Dipper retries.
        sqlx::query(
            "INSERT INTO pending_rca_proposals (id, signed_payload, version, status, created_at, updated_at)
             VALUES ($1, $2, $3, 'pending', NOW(), NOW())
             ON CONFLICT (id) DO NOTHING",
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
