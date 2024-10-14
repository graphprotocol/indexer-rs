// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc};

use anyhow::bail;
use async_graphql::{Context, FieldResult, Object, SimpleObject};
use base64::{engine::general_purpose::STANDARD, Engine};
use indexer_dips::alloy::dyn_abi::Eip712Domain;
use indexer_dips::{
    alloy::core::primitives::Address, alloy_rlp::Decodable, SignedIndexingAgreementVoucher,
    SubgraphIndexingVoucherMetadata,
};

use crate::database::dips::AgreementStore;

pub enum NetworkProtocol {
    ArbitrumMainnet,
}

impl FromStr for NetworkProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let p = match s {
            "arbitrum-mainnet" => NetworkProtocol::ArbitrumMainnet,
            _ => bail!("unknown network protocol"),
        };

        Ok(p)
    }
}

#[derive(SimpleObject, Debug, Clone, PartialEq)]
pub struct Agreement {
    signature: String,
    data: String,
    protocol_network: String,
}

impl TryInto<SignedIndexingAgreementVoucher> for Agreement {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<SignedIndexingAgreementVoucher, Self::Error> {
        let rlp_bytes = STANDARD.decode(self.data)?;
        let signed_voucher = SignedIndexingAgreementVoucher::decode(&mut rlp_bytes.as_ref())?;

        Ok(signed_voucher)
    }
}

#[derive(SimpleObject, Debug, Clone)]
pub struct Price {
    price_per_block: String,
    chain_id: String,
    protocol_network: String,
}

#[derive(Debug)]
pub struct AgreementQuery {}

#[Object]
impl AgreementQuery {
    pub async fn get_agreement<'a>(
        &self,
        ctx: &'a Context<'_>,
        signature: String,
    ) -> FieldResult<Option<Agreement>> {
        let store: &Arc<dyn AgreementStore> = ctx.data()?;

        store
            .get_by_signature(signature)
            .await
            .map_err(async_graphql::Error::from)
    }

    pub async fn get_price<'a>(
        &self,
        ctx: &'a Context<'_>,
        protocol_network: String,
        chain_id: String,
    ) -> FieldResult<Option<Price>> {
        let prices: &Vec<Price> = ctx.data()?;

        let p = prices
            .iter()
            .find(|p| p.protocol_network.eq(&protocol_network) && p.chain_id.eq(&chain_id));

        Ok(p.cloned())
    }

    pub async fn get_all_prices<'a>(&self, ctx: &'a Context<'_>) -> FieldResult<Vec<Price>> {
        let prices: &Vec<Price> = ctx.data()?;

        Ok(prices.clone())
    }
}

#[derive(Debug)]
pub struct AgreementMutation {
    pub expected_payee: Address,
    pub allowed_payers: Vec<Address>,
    pub domain: Eip712Domain,
}

#[Object]
impl AgreementMutation {
    pub async fn create_agreement<'a>(
        &self,
        ctx: &'a Context<'_>,
        // data should be the signed voucher, eip712 signed, rlp and base64 encoded.
        data: String,
    ) -> FieldResult<Agreement> {
        let store: &Arc<dyn AgreementStore> = ctx.data()?;

        validate_and_create_agreement(
            store.clone(),
            &self.domain,
            &self.expected_payee,
            &self.allowed_payers,
            data,
        )
        .await
        .map_err(async_graphql::Error::from)
    }

    pub async fn cancel_agreement<'a>(
        &self,
        ctx: &'a Context<'_>,
        signature: String,
    ) -> FieldResult<String> {
        let store: &Arc<dyn AgreementStore> = ctx.data()?;

        store
            .cancel_agreement(signature)
            .await
            .map_err(async_graphql::Error::from)
    }
}

async fn validate_and_create_agreement(
    store: Arc<dyn AgreementStore>,
    domain: &Eip712Domain,
    expected_payee: &Address,
    allowed_payers: impl AsRef<[Address]>,
    agreement: String,
) -> anyhow::Result<Agreement> {
    let rlp_bs = STANDARD.decode(agreement.clone())?;
    let voucher = SignedIndexingAgreementVoucher::decode(&mut rlp_bs.as_ref())?;
    let metadata = SubgraphIndexingVoucherMetadata::decode(&mut voucher.voucher.metadata.as_ref())?;

    voucher.validate(domain, expected_payee, allowed_payers)?;

    let agreement = Agreement {
        signature: voucher.signature.to_string(),
        data: agreement,
        protocol_network: metadata.protocolNetwork.to_string(),
    };

    store
        .create_agreement(voucher.signature.to_string(), agreement)
        .await
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use alloy::signers::local::PrivateKeySigner;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use indexer_dips::{
        alloy::core::primitives::{Address, FixedBytes, U256},
        alloy_rlp::{self},
        IndexingAgreementVoucher, SubgraphIndexingVoucherMetadata,
    };
    use thegraph_core::attestation::eip712_domain;

    use crate::database::dips::InMemoryAgreementStore;

    #[tokio::test]
    async fn test_validate_and_create_agreement() -> anyhow::Result<()> {
        let payee = PrivateKeySigner::random();
        let payee_addr = payee.address();
        let payer = PrivateKeySigner::random();
        let payer_addr = payer.address();

        let metadata = SubgraphIndexingVoucherMetadata {
            pricePerBlock: U256::from(10000_u64),
            protocolNetwork: FixedBytes::left_padding_from("arbitrum-one".as_bytes()),
            chainId: FixedBytes::left_padding_from("mainnet".as_bytes()),
        };

        let voucher = IndexingAgreementVoucher {
            payer: payer_addr,
            payee: payee_addr,
            service: Address(FixedBytes::ZERO),
            maxInitialAmount: U256::from(10000_u64),
            maxOngoingAmountPerEpoch: U256::from(10000_u64),
            deadline: 1000,
            maxEpochsPerCollection: 1000,
            minEpochsPerCollection: 1000,
            durationEpochs: 1000,
            metadata: alloy_rlp::encode(metadata).into(),
        };
        let domain = eip712_domain(0, Address::ZERO);

        let voucher = voucher.sign(&domain, payer)?;
        let rlp_voucher = alloy_rlp::encode(voucher.clone());
        let b64 = STANDARD.encode(rlp_voucher);

        let store = Arc::new(InMemoryAgreementStore::default());

        let actual = super::validate_and_create_agreement(
            store,
            &domain,
            &payee_addr,
            vec![payer_addr],
            b64,
        )
        .await
        .unwrap();

        let actual_voucher = actual.try_into().unwrap();
        assert_eq!(voucher, actual_voucher);
        Ok(())
    }
}
