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

use std::{any::Any, time::Duration};

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

    async fn count_since(&self, window: Duration) -> Result<u64, DipsError> {
        // NOW() is the database clock, so the window is unaffected by host clock skew.
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pending_rca_proposals
             WHERE created_at >= NOW() - ($1 * interval '1 second')",
        )
        .bind(window.as_secs() as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| DipsError::UnknownError(e.into()))?;

        Ok(count.max(0) as u64)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Insert a proposal row whose created_at is `hours_ago` before the DB clock.
    async fn insert_at(pool: &PgPool, id: Uuid, hours_ago: i64) {
        sqlx::query(
            "INSERT INTO pending_rca_proposals (id, signed_payload, version, status, created_at, updated_at)
             VALUES ($1, $2, 2, 'pending', NOW() - ($3 * interval '1 hour'), NOW())",
        )
        .bind(id)
        .bind(vec![1u8, 2, 3])
        .bind(hours_ago)
        .execute(pool)
        .await
        .unwrap();
    }

    // Exercises the rolling-window SQL against a real Postgres (testcontainers);
    // the in-memory store can't, since it ignores the window.
    #[tokio::test]
    async fn count_since_excludes_rows_outside_the_window() {
        let test_db = test_assets::setup_shared_test_db().await;
        let store = PsqlRcaStore {
            pool: test_db.pool.clone(),
        };
        insert_at(&test_db.pool, Uuid::from_u128(1), 1).await; // inside 24h
        insert_at(&test_db.pool, Uuid::from_u128(2), 25).await; // outside 24h

        let count = store
            .count_since(Duration::from_secs(24 * 60 * 60))
            .await
            .unwrap();

        assert_eq!(count, 1, "only the row inside the window should count");
    }
}
