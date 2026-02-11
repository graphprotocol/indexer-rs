// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Storage abstraction for RCA proposals.
//!
//! This module defines the [`RcaStore`] trait for persisting validated RCA proposals.
//! The indexer-service validates incoming proposals and stores them; the indexer-agent
//! (a separate TypeScript process) queries this table to decide on-chain acceptance.
//!
//! # Database Schema
//!
//! Proposals are stored in the `pending_rca_proposals` table:
//!
//! | Column         | Type        | Description                              |
//! |----------------|-------------|------------------------------------------|
//! | id             | UUID        | Agreement ID from the RCA                |
//! | signed_payload | BYTEA       | Raw ABI-encoded SignedRCA bytes          |
//! | version        | SMALLINT    | Protocol version (currently 2)           |
//! | status         | VARCHAR(20) | "pending", "accepted", "rejected", etc.  |
//! | created_at     | TIMESTAMPTZ | When the proposal was received           |
//! | updated_at     | TIMESTAMPTZ | Last status change                       |
//!
//! # Implementations
//!
//! - [`InMemoryRcaStore`] - In-memory store for unit tests
//! - [`PsqlRcaStore`](crate::database::PsqlRcaStore) - PostgreSQL implementation

use std::any::Any;

use async_trait::async_trait;
use uuid::Uuid;

use crate::DipsError;

/// Store for RCA (RecurringCollectionAgreement) proposals.
///
/// Stores validated RCA proposals. The indexer agent queries this table,
/// validates allocation availability, and submits on-chain acceptance.
#[async_trait]
pub trait RcaStore: Sync + Send + std::fmt::Debug {
    /// Store a validated RCA proposal.
    ///
    /// Only called after successful validation (signature, IPFS, pricing).
    async fn store_rca(
        &self,
        agreement_id: Uuid,
        signed_rca: Vec<u8>,
        version: u64,
    ) -> Result<(), DipsError>;

    /// Downcast to concrete type for testing.
    fn as_any(&self) -> &dyn Any;
}

/// In-memory implementation of RcaStore for testing.
#[derive(Default, Debug)]
pub struct InMemoryRcaStore {
    pub data: tokio::sync::RwLock<Vec<(Uuid, Vec<u8>, u64)>>,
}

#[async_trait]
impl RcaStore for InMemoryRcaStore {
    async fn store_rca(
        &self,
        agreement_id: Uuid,
        signed_rca: Vec<u8>,
        version: u64,
    ) -> Result<(), DipsError> {
        self.data
            .write()
            .await
            .push((agreement_id, signed_rca, version));
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Test store that always fails.
#[derive(Default, Debug)]
pub struct FailingRcaStore;

#[async_trait]
impl RcaStore for FailingRcaStore {
    async fn store_rca(
        &self,
        _agreement_id: Uuid,
        _signed_rca: Vec<u8>,
        _version: u64,
    ) -> Result<(), DipsError> {
        Err(DipsError::UnknownError(anyhow::anyhow!(
            "database connection failed (test store)"
        )))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_rca() {
        // Arrange
        let store = InMemoryRcaStore::default();
        let id = Uuid::now_v7();
        let blob = vec![1, 2, 3, 4, 5];

        // Act
        store.store_rca(id, blob.clone(), 2).await.unwrap();

        // Assert
        let data = store.data.read().await;
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].0, id);
        assert_eq!(data[0].1, blob);
        assert_eq!(data[0].2, 2);
    }

    #[tokio::test]
    async fn test_store_multiple_rcas() {
        // Arrange
        let store = InMemoryRcaStore::default();
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let blob1 = vec![1, 2, 3];
        let blob2 = vec![4, 5, 6];

        // Act
        store.store_rca(id1, blob1.clone(), 2).await.unwrap();
        store.store_rca(id2, blob2.clone(), 2).await.unwrap();

        // Assert
        let data = store.data.read().await;
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].0, id1);
        assert_eq!(data[0].1, blob1);
        assert_eq!(data[1].0, id2);
        assert_eq!(data[1].1, blob2);
    }

    #[tokio::test]
    async fn test_failing_rca_store() {
        // Arrange
        let store = FailingRcaStore;
        let id = Uuid::now_v7();
        let blob = vec![1, 2, 3];

        // Act
        let result = store.store_rca(id, blob, 2).await;

        // Assert
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DipsError::UnknownError(_)),
            "Expected UnknownError, got: {:?}",
            err
        );
    }
}
