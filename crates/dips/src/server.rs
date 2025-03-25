// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use indexer_monitor::EscrowAccounts;
use thegraph_core::alloy::primitives::{Address, ChainId};
use tonic::{Request, Response, Status};

use crate::{
    ipfs::IpfsFetcher,
    price::PriceCalculator,
    proto::indexer::graphprotocol::indexer::dips::{
        indexer_dips_service_server::IndexerDipsService, CancelAgreementRequest,
        CancelAgreementResponse, ProposalResponse, SubmitAgreementProposalRequest,
        SubmitAgreementProposalResponse,
    },
    signers::SignerValidator,
    store::AgreementStore,
    validate_and_cancel_agreement, validate_and_create_agreement, PROTOCOL_VERSION,
    dips_agreement_eip712_domain, dips_cancellation_eip712_domain,
};

#[derive(Debug)]
pub struct DipsServerContext {
    pub store: Arc<dyn AgreementStore>,
    pub ipfs_fetcher: Arc<dyn IpfsFetcher>,
    pub price_calculator: PriceCalculator,
    pub signer_validator: Arc<dyn SignerValidator>,
}

impl DipsServerContext {
    #[cfg(test)]
    pub fn for_testing() -> Arc<Self> {
        use std::sync::Arc;

        use crate::{ipfs::TestIpfsClient, signers, test::InMemoryAgreementStore};

        Arc::new(DipsServerContext {
            store: Arc::new(InMemoryAgreementStore::default()),
            ipfs_fetcher: Arc::new(TestIpfsClient::mainnet()),
            price_calculator: PriceCalculator::for_testing(),
            signer_validator: Arc::new(signers::NoopSignerValidator),
        })
    }

    #[cfg(test)]
    pub async fn for_testing_mocked_accounts(accounts: EscrowAccounts) -> Arc<Self> {
        use crate::{ipfs::TestIpfsClient, signers, test::InMemoryAgreementStore};

        Arc::new(DipsServerContext {
            store: Arc::new(InMemoryAgreementStore::default()),
            ipfs_fetcher: Arc::new(TestIpfsClient::mainnet()),
            price_calculator: PriceCalculator::for_testing(),
            signer_validator: Arc::new(signers::EscrowSignerValidator::mock(accounts).await),
        })
    }
}

#[derive(Debug)]
pub struct DipsServer {
    pub ctx: Arc<DipsServerContext>,
    pub expected_payee: Address,
    pub allowed_payers: Vec<Address>,
    pub chain_id: ChainId,
}

#[async_trait]
impl IndexerDipsService for DipsServer {
    async fn submit_agreement_proposal(
        &self,
        request: Request<SubmitAgreementProposalRequest>,
    ) -> Result<Response<SubmitAgreementProposalResponse>, Status> {
        let SubmitAgreementProposalRequest {
            version,
            signed_voucher,
        } = request.into_inner();

        // Ensure the version is 1
        if version != PROTOCOL_VERSION {
            return Err(Status::invalid_argument("invalid version"));
        }

        // TODO: Validate that:
        // - The price is over the configured minimum price
        // - The subgraph deployment is for a chain we support
        // - The subgraph deployment is available on IPFS
        validate_and_create_agreement(
            self.ctx.clone(),
            &dips_agreement_eip712_domain(self.chain_id),
            &self.expected_payee,
            &self.allowed_payers,
            signed_voucher,
        )
        .await?;

        Ok(Response::new(SubmitAgreementProposalResponse {
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
            version,
            signed_cancellation,
        } = request.into_inner();

        if version != 1 {
            return Err(Status::invalid_argument("invalid version"));
        }

        validate_and_cancel_agreement(
            self.ctx.store.clone(),
            &dips_cancellation_eip712_domain(self.chain_id),
            signed_cancellation,
        )
        .await
        .map_err(Into::<tonic::Status>::into)?;

        Ok(tonic::Response::new(CancelAgreementResponse {}))
    }
}
