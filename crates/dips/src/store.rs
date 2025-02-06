// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    DipsError, SignedCancellationRequest, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};

#[async_trait]
pub trait AgreementStore: Sync + Send + std::fmt::Debug {
    async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<(SignedIndexingAgreementVoucher, bool)>, DipsError>;
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
    pub data: tokio::sync::RwLock<HashMap<Uuid, (SignedIndexingAgreementVoucher, bool)>>,
}

#[async_trait]
impl AgreementStore for InMemoryAgreementStore {
    async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<(SignedIndexingAgreementVoucher, bool)>, DipsError> {
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
        _medatadata: SubgraphIndexingVoucherMetadata,
    ) -> Result<(), DipsError> {
        self.data
            .try_write()
            .map_err(|e| DipsError::UnknownError(e.into()))?
            .insert(
                Uuid::from_bytes(agreement.voucher.agreement_id.into()),
                (agreement.clone(), false),
            );

        Ok(())
    }
    async fn cancel_agreement(
        &self,
        signed_cancellation: SignedCancellationRequest,
    ) -> Result<Uuid, DipsError> {
        let id = Uuid::from_bytes(signed_cancellation.request.agreement_id.into());

        let agreement = {
            let read_lock = self
                .data
                .try_read()
                .map_err(|e| DipsError::UnknownError(e.into()))?;
            read_lock
                .get(&id)
                .cloned()
                .ok_or(DipsError::AgreementNotFound)?
        };

        let mut write_lock = self
            .data
            .try_write()
            .map_err(|e| DipsError::UnknownError(e.into()))?;
        write_lock.insert(id, (agreement.0, true));

        Ok(id)
    }
}
