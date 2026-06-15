// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Storage abstraction for RCA proposals. [`RcaStore`] persists validated
//! proposals to the shared `pending_rca_proposals` table; indexer-rs writes them
//! and the indexer-agent reads them and advances `status`. See the migration.

use std::{any::Any, time::Duration};

use async_trait::async_trait;
use uuid::Uuid;

use crate::DipsError;

/// Lifecycle statuses for a `pending_rca_proposals` row, shared with the
/// indexer-agent and enforced by a CHECK constraint. indexer-rs writes only
/// `pending`; the agent advances a row to accepted, completed, or rejected.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_ACCEPTED: &str = "accepted";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_REJECTED: &str = "rejected";

/// A proposal already on record, returned by [`RcaStore::lookup`]. Carries the
/// lifecycle `status` and raw `signed_payload` so the early replay check can
/// compare byte-for-byte against a re-sent proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredProposal {
    pub status: String,
    pub signed_payload: Vec<u8>,
}

/// Store for validated RCA proposals: indexer-rs writes them here and the
/// indexer-agent reads them to decide on-chain acceptance.
#[async_trait]
pub trait RcaStore: Sync + Send + std::fmt::Debug {
    /// Store a validated RCA proposal. MUST be idempotent: storing the same
    /// `agreement_id` twice must both succeed, so a Dipper re-send after a
    /// timeout or network partition doesn't fail on a duplicate.
    async fn store_rca(
        &self,
        agreement_id: Uuid,
        signed_rca: Vec<u8>,
        version: u64,
    ) -> Result<(), DipsError>;

    /// Look up a stored proposal by its deterministic agreement ID, `Ok(None)`
    /// when absent. The early replay check calls this before the IPFS fetch so a
    /// re-sent proposal skips the download.
    async fn lookup(&self, agreement_id: Uuid) -> Result<Option<StoredProposal>, DipsError>;

    /// Count live agreements (status `pending` or `accepted`) within the trailing
    /// `window`; rejected and expired proposals are excluded. The in-memory test
    /// store has no status, so it ignores the window and counts every entry.
    async fn count_since(&self, window: Duration) -> Result<u64, DipsError>;

    /// Downcast to concrete type for testing.
    fn as_any(&self) -> &dyn Any;
}

/// One in-memory row: (id, signed payload, version, status).
type InMemoryRow = (Uuid, Vec<u8>, u64, String);

/// In-memory implementation of RcaStore for testing. Each row carries a status
/// string (defaulting to "pending" on insert) so the replay check can be
/// exercised against accepted and rejected rows in tests.
#[derive(Default, Debug)]
pub struct InMemoryRcaStore {
    pub data: tokio::sync::RwLock<Vec<InMemoryRow>>,
}

#[async_trait]
impl RcaStore for InMemoryRcaStore {
    async fn store_rca(
        &self,
        agreement_id: Uuid,
        signed_rca: Vec<u8>,
        version: u64,
    ) -> Result<(), DipsError> {
        let mut data = self.data.write().await;
        // Idempotent: skip if already exists
        if !data.iter().any(|(id, _, _, _)| *id == agreement_id) {
            data.push((
                agreement_id,
                signed_rca,
                version,
                STATUS_PENDING.to_string(),
            ));
        }
        Ok(())
    }

    async fn lookup(&self, agreement_id: Uuid) -> Result<Option<StoredProposal>, DipsError> {
        let data = self.data.read().await;
        Ok(data
            .iter()
            .find(|(id, _, _, _)| *id == agreement_id)
            .map(|(_, payload, _, status)| StoredProposal {
                status: status.clone(),
                signed_payload: payload.clone(),
            }))
    }

    async fn count_since(&self, _window: Duration) -> Result<u64, DipsError> {
        Ok(self.data.read().await.len() as u64)
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

    async fn lookup(&self, _agreement_id: Uuid) -> Result<Option<StoredProposal>, DipsError> {
        Err(DipsError::UnknownError(anyhow::anyhow!(
            "database connection failed (test store)"
        )))
    }

    async fn count_since(&self, _window: Duration) -> Result<u64, DipsError> {
        Err(DipsError::UnknownError(anyhow::anyhow!(
            "database connection failed (test store)"
        )))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Test double whose `lookup` errors but whose `store_rca` succeeds, to exercise
/// the fail-open path: a lookup error must be swallowed so validation still runs.
#[derive(Default, Debug)]
pub struct LookupFailsStore;

#[async_trait]
impl RcaStore for LookupFailsStore {
    async fn store_rca(
        &self,
        _agreement_id: Uuid,
        _signed_rca: Vec<u8>,
        _version: u64,
    ) -> Result<(), DipsError> {
        Ok(())
    }

    async fn lookup(&self, _agreement_id: Uuid) -> Result<Option<StoredProposal>, DipsError> {
        Err(DipsError::UnknownError(anyhow::anyhow!(
            "lookup failed (test store)"
        )))
    }

    async fn count_since(&self, _window: Duration) -> Result<u64, DipsError> {
        Ok(0)
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

    #[tokio::test]
    async fn test_store_rca_idempotent() {
        // Arrange
        let store = InMemoryRcaStore::default();
        let id = Uuid::now_v7();
        let blob = vec![1, 2, 3, 4, 5];

        // Act - store same ID twice
        let result1 = store.store_rca(id, blob.clone(), 2).await;
        let result2 = store.store_rca(id, blob.clone(), 2).await;

        // Assert - both succeed, only one entry stored
        assert!(result1.is_ok(), "First store should succeed");
        assert!(result2.is_ok(), "Second store (retry) should also succeed");

        let data = store.data.read().await;
        assert_eq!(data.len(), 1, "Duplicate should not create second entry");
        assert_eq!(data[0].0, id);
    }

    #[tokio::test]
    async fn test_lookup_absent_returns_none() {
        // Arrange
        let store = InMemoryRcaStore::default();

        // Act
        let found = store.lookup(Uuid::now_v7()).await.unwrap();

        // Assert
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_lookup_returns_pending_payload() {
        // Arrange
        let store = InMemoryRcaStore::default();
        let id = Uuid::now_v7();
        let blob = vec![9, 8, 7, 6];
        store.store_rca(id, blob.clone(), 2).await.unwrap();

        // Act
        let found = store.lookup(id).await.unwrap();

        // Assert
        assert_eq!(
            found,
            Some(StoredProposal {
                status: "pending".to_string(),
                signed_payload: blob,
            })
        );
    }

    #[tokio::test]
    async fn test_failing_store_lookup_errors() {
        // Arrange
        let store = FailingRcaStore;

        // Act
        let result = store.lookup(Uuid::now_v7()).await;

        // Assert
        assert!(matches!(result, Err(DipsError::UnknownError(_))));
    }
}
