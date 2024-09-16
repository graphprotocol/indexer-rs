// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use axum::async_trait;

use crate::routes::dips::Agreement;

#[async_trait]
pub trait AgreementStore: Sync + Send {
    async fn get_by_signature(&self, signature: String) -> anyhow::Result<Option<Agreement>>;
    async fn create_agreement(
        &self,
        signature: String,
        data: Agreement,
    ) -> anyhow::Result<Agreement>;
    async fn cancel_agreement(&self, signature: String) -> anyhow::Result<String>;
}

pub struct InMemoryAgreementStore {
    pub data: tokio::sync::RwLock<HashMap<String, Agreement>>,
}

#[async_trait]
impl AgreementStore for InMemoryAgreementStore {
    async fn get_by_signature(&self, signature: String) -> anyhow::Result<Option<Agreement>> {
        Ok(self.data.try_read()?.get(&signature).cloned())
    }
    async fn create_agreement(
        &self,
        signature: String,
        agreement: Agreement,
    ) -> anyhow::Result<Agreement> {
        self.data.try_write()?.insert(signature, agreement.clone());

        Ok(agreement)
    }
    async fn cancel_agreement(&self, signature: String) -> anyhow::Result<String> {
        self.data.try_write()?.remove(&signature);

        Ok(signature.clone())
    }
}
