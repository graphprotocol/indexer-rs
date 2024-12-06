// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{SignedCancellationRequest, SignedIndexingAgreementVoucher};

#[async_trait]
pub trait AgreementStore: Sync + Send + std::fmt::Debug {
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

#[derive(Default, Debug)]
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
