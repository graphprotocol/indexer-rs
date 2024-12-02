// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc, time::Duration};

use crate::{
    proto::graphprotocol::indexer::dips::*, store::AgreementStore, validate_and_cancel_agreement,
    validate_and_create_agreement, DipsError,
};
use anyhow::anyhow;
use async_trait::async_trait;
use thegraph_core::alloy::{dyn_abi::Eip712Domain, primitives::Address};
use uuid::Uuid;

#[derive(Debug)]
pub struct DipsServer {
    pub agreement_store: Arc<dyn AgreementStore>,
    pub expected_payee: Address,
    pub allowed_payers: Vec<Address>,
    pub domain: Eip712Domain,
    pub cancel_voucher_time_tolerance: Duration,
}

#[async_trait]
impl agreement_service_server::AgreementService for DipsServer {
    async fn create_agreement(
        &self,
        request: tonic::Request<CreateAgreementRequest>,
    ) -> std::result::Result<tonic::Response<CreateAgreementResponse>, tonic::Status> {
        let CreateAgreementRequest { id, signed_voucher } = request.into_inner();
        let uid = Uuid::from_str(&id).map_err(|_| {
            Into::<tonic::Status>::into(DipsError::from(anyhow!("failed to parse uuid")))
        })?;

        validate_and_create_agreement(
            self.agreement_store.clone(),
            &self.domain,
            uid,
            &self.expected_payee,
            &self.allowed_payers,
            signed_voucher,
        )
        .await
        .map_err(Into::<tonic::Status>::into)?;

        Ok(tonic::Response::new(CreateAgreementResponse {
            uuid: uid.to_string(),
        }))
    }

    async fn cancel_agreement(
        &self,
        request: tonic::Request<CancelAgreementRequest>,
    ) -> std::result::Result<tonic::Response<AgreementCanellationResponse>, tonic::Status> {
        let CancelAgreementRequest { id, signed_voucher } = request.into_inner();
        let uid = Uuid::from_str(&id).map_err(|_| {
            Into::<tonic::Status>::into(DipsError::from(anyhow!("failed to parse uuid")))
        })?;

        validate_and_cancel_agreement(
            self.agreement_store.clone(),
            &self.domain,
            uid,
            signed_voucher,
            self.cancel_voucher_time_tolerance,
        )
        .await
        .map_err(Into::<tonic::Status>::into)?;

        Ok(tonic::Response::new(AgreementCanellationResponse {
            uuid: uid.to_string(),
        }))
    }
    async fn get_agreement_by_id(
        &self,
        _request: tonic::Request<GetAgreementByIdRequest>,
    ) -> std::result::Result<tonic::Response<GetAgreementByIdResponse>, tonic::Status> {
        todo!()
    }

    async fn get_price(
        &self,
        _request: tonic::Request<PriceRequest>,
    ) -> std::result::Result<tonic::Response<PriceResponse>, tonic::Status> {
        todo!()
    }
}
