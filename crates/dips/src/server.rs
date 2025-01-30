// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use async_trait::async_trait;
use thegraph_core::alloy::{dyn_abi::Eip712Domain, primitives::Address};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::{
    proto::indexer::graphprotocol::indexer::dips::{
        dips_service_server::DipsService, CancelAgreementRequest, CancelAgreementResponse,
        ProposalResponse, SubmitAgreementProposalRequest, SubmitAgreementProposalResponse,
    },
    store::AgreementStore,
    validate_and_cancel_agreement, validate_and_create_agreement, DipsError,
};

#[derive(Debug)]
pub struct DipsServer {
    pub agreement_store: Arc<dyn AgreementStore>,
    pub expected_payee: Address,
    pub allowed_payers: Vec<Address>,
    pub domain: Eip712Domain,
    pub cancel_voucher_time_tolerance: Duration,
}

#[async_trait]
impl DipsService for DipsServer {
    async fn submit_agreement_proposal(
        &self,
        request: Request<SubmitAgreementProposalRequest>,
    ) -> Result<Response<SubmitAgreementProposalResponse>, Status> {
        let SubmitAgreementProposalRequest {
            agreement_id,
            signed_voucher,
        } = request.into_inner();
        let bs: [u8; 16] = agreement_id.as_slice().try_into().map_err(|_| {
            Into::<tonic::Status>::into(DipsError::from(anyhow!("failed to parse uuid")))
        })?;
        let uid = Uuid::from_bytes(bs);

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

        Ok(tonic::Response::new(SubmitAgreementProposalResponse {
            response: ProposalResponse::Accept.into(),
        }))
    }
    /// *
    /// Request to cancel an existing _indexing agreement_.
    async fn cancel_agreement(
        &self,
        request: Request<CancelAgreementRequest>,
    ) -> Result<Response<CancelAgreementResponse>, Status> {
        let CancelAgreementRequest {
            agreement_id,
            signature: _,
        } = request.into_inner();

        let bs: [u8; 16] = agreement_id.as_slice().try_into().map_err(|_| {
            Into::<tonic::Status>::into(DipsError::from(anyhow!("failed to parse uuid")))
        })?;
        let uid = Uuid::from_bytes(bs);

        validate_and_cancel_agreement(
            self.agreement_store.clone(),
            &self.domain,
            uid,
            vec![],
            self.cancel_voucher_time_tolerance,
        )
        .await
        .map_err(Into::<tonic::Status>::into)?;

        Ok(tonic::Response::new(CancelAgreementResponse {}))
    }

    // async fn create_agreement(
    //     &self,
    //     request: tonic::Request<CreateAgreementRequest>,
    // ) -> std::result::Result<tonic::Response<CreateAgreementResponse>, tonic::Status> {
    // }

    // async fn cancel_agreement(
    //     &self,
    //     request: tonic::Request<CancelAgreementRequest>,
    // ) -> std::result::Result<tonic::Response<AgreementCanellationResponse>, tonic::Status> {
    //     let CancelAgreementRequest { id, signed_voucher } = request.into_inner();
    //     let uid = Uuid::from_str(&id).map_err(|_| {
    //         Into::<tonic::Status>::into(DipsError::from(anyhow!("failed to parse uuid")))
    //     })?;

    //     validate_and_cancel_agreement(
    //         self.agreement_store.clone(),
    //         &self.domain,
    //         uid,
    //         signed_voucher,
    //         self.cancel_voucher_time_tolerance,
    //     )
    //     .await
    //     .map_err(Into::<tonic::Status>::into)?;

    //     Ok(tonic::Response::new(AgreementCanellationResponse {
    //         uuid: uid.to_string(),
    //     }))
    // }
    // async fn get_agreement_by_id(
    //     &self,
    //     _request: tonic::Request<GetAgreementByIdRequest>,
    // ) -> std::result::Result<tonic::Response<GetAgreementByIdResponse>, tonic::Status> {
    //     todo!()
    // }

    // async fn get_price(
    //     &self,
    //     _request: tonic::Request<PriceRequest>,
    // ) -> std::result::Result<tonic::Response<PriceResponse>, tonic::Status> {
    //     todo!()
    // }
}
