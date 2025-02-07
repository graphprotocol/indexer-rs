// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use async_trait::async_trait;
use build_info::chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    DipsError, SignedCancellationRequest, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};

#[derive(Debug, Clone)]
pub struct StoredIndexingAgreement {
    pub voucher: SignedIndexingAgreementVoucher,
    pub metadata: SubgraphIndexingVoucherMetadata,
    pub cancelled: bool,
    pub current_allocation_id: Option<String>,
    pub last_allocation_id: Option<String>,
    pub last_payment_collected_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait AgreementStore: Sync + Send + std::fmt::Debug {
    async fn get_by_id(&self, id: Uuid) -> Result<Option<StoredIndexingAgreement>, DipsError>;
    async fn create_agreement(
        &self,
        agreement: SignedIndexingAgreementVoucher,
        metadata: SubgraphIndexingVoucherMetadata,
    ) -> Result<(), DipsError>;
    async fn cancel_agreement(
        &self,
        signed_cancellation: SignedCancellationRequest,
    ) -> Result<Uuid, DipsError>;
}

#[derive(Default, Debug)]
pub struct InMemoryAgreementStore {
    pub data: tokio::sync::RwLock<HashMap<Uuid, StoredIndexingAgreement>>,
}

#[async_trait]
impl AgreementStore for InMemoryAgreementStore {
    async fn get_by_id(&self, id: Uuid) -> Result<Option<StoredIndexingAgreement>, DipsError> {
        Ok(self
            .data
            .try_read()
            .map_err(|e| DipsError::UnknownError(e.into()))?
            .get(&id)
            .cloned())
    }
    async fn create_agreement(
        &self,
        agreement: SignedIndexingAgreementVoucher,
        metadata: SubgraphIndexingVoucherMetadata,
    ) -> Result<(), DipsError> {
        let id = Uuid::from_bytes(agreement.voucher.agreement_id.into());
        let stored_agreement = StoredIndexingAgreement {
            voucher: agreement,
            metadata,
            cancelled: false,
            current_allocation_id: None,
            last_allocation_id: None,
            last_payment_collected_at: None,
        };
        self.data
            .try_write()
            .map_err(|e| DipsError::UnknownError(e.into()))?
            .insert(id, stored_agreement);

        Ok(())
    }
    async fn cancel_agreement(
        &self,
        signed_cancellation: SignedCancellationRequest,
    ) -> Result<Uuid, DipsError> {
        let id = Uuid::from_bytes(signed_cancellation.request.agreement_id.into());

        let mut agreement = {
            let read_lock = self
                .data
                .try_read()
                .map_err(|e| DipsError::UnknownError(e.into()))?;
            read_lock
                .get(&id)
                .cloned()
                .ok_or(DipsError::AgreementNotFound)?
        };

        if agreement.cancelled {
            return Err(DipsError::AgreementCancelled);
        }

        agreement.cancelled = true;

        let mut write_lock = self
            .data
            .try_write()
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        write_lock.insert(id, agreement);

        Ok(id)
    }
}
