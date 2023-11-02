// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use anyhow::Result;
use async_trait::async_trait;
use sqlx::PgPool;
use tap_core::adapters::rav_storage_adapter::RAVStorageAdapter as RAVStorageAdapterTrait;
use tap_core::tap_manager::SignedRAV;
use thiserror::Error;

#[derive(Debug)]
pub struct RAVStorageAdapter {
    pgpool: PgPool,
    allocation_id: Address,
    sender: Address,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Error in RAVStorageAdapter: {error}")]
    AdapterError { error: String },
}

#[async_trait]
impl RAVStorageAdapterTrait for RAVStorageAdapter {
    type AdapterError = AdapterError;

    async fn update_last_rav(&self, rav: SignedRAV) -> Result<(), Self::AdapterError> {
        let _fut = sqlx::query!(
            r#"
                INSERT INTO scalar_tap_ravs (allocation_id, sender_address, rav)
                VALUES ($1, $2, $3)
                ON CONFLICT (allocation_id, sender_address)
                DO UPDATE SET rav = $3
            "#,
            self.allocation_id
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            self.sender.to_string().trim_start_matches("0x").to_owned(),
            serde_json::to_value(rav).map_err(|e| AdapterError::AdapterError {
                error: e.to_string()
            })?
        )
        .execute(&self.pgpool)
        .await
        .map_err(|e| AdapterError::AdapterError {
            error: e.to_string(),
        })?;
        Ok(())
    }

    async fn last_rav(&self) -> Result<Option<SignedRAV>, Self::AdapterError> {
        let latest_rav = sqlx::query!(
            r#"
                SELECT rav
                FROM scalar_tap_ravs
                WHERE allocation_id = $1 AND sender_address = $2
            "#,
            self.allocation_id
                .to_string()
                .trim_start_matches("0x")
                .to_owned(),
            self.sender.to_string().trim_start_matches("0x").to_owned()
        )
        .fetch_optional(&self.pgpool)
        .await
        .map(|r| r.map(|r| r.rav))
        .map_err(|e| AdapterError::AdapterError {
            error: e.to_string(),
        })?;
        match latest_rav {
            Some(latest_rav) => {
                Ok(
                    serde_json::from_value(latest_rav).map_err(|e| AdapterError::AdapterError {
                        error: e.to_string(),
                    }),
                )?
            }
            None => Ok(None),
        }
    }
}

impl RAVStorageAdapter {
    pub fn new(pgpool: PgPool, allocation_id: Address, sender: Address) -> Self {
        Self {
            pgpool,
            allocation_id,
            sender,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::tap::test_utils::{create_rav, ALLOCATION_ID, SENDER};
    use tap_core::adapters::rav_storage_adapter::RAVStorageAdapter as RAVStorageAdapterTrait;

    #[sqlx::test(migrations = "../migrations")]
    async fn update_and_retrieve_rav(pool: PgPool) {
        let timestamp_ns = u64::MAX - 10;
        let value_aggregate = u128::MAX;
        let rav_storage_adapter = RAVStorageAdapter::new(pool.clone(), *ALLOCATION_ID, SENDER.1);

        // Insert a rav
        let mut new_rav = create_rav(
            *ALLOCATION_ID,
            SENDER.0.clone(),
            timestamp_ns,
            value_aggregate,
        )
        .await;
        rav_storage_adapter
            .update_last_rav(new_rav.clone())
            .await
            .unwrap();

        // Should trigger a retrieve_last_rav So eventually the last rav should be the one
        // we inserted
        let last_rav = rav_storage_adapter.last_rav().await.unwrap();
        assert_eq!(new_rav, last_rav.unwrap());

        // Update the RAV 3 times in quick succession
        for i in 0..3 {
            new_rav = create_rav(
                *ALLOCATION_ID,
                SENDER.0.clone(),
                timestamp_ns + i,
                value_aggregate - (i as u128),
            )
            .await;
            rav_storage_adapter
                .update_last_rav(new_rav.clone())
                .await
                .unwrap();
        }

        // Check that the last rav is the last one we inserted
        let last_rav = rav_storage_adapter.last_rav().await.unwrap();
        assert_eq!(new_rav, last_rav.unwrap());
    }
}
