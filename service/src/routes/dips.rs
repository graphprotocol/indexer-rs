// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::Arc};

use anyhow::bail;
use async_graphql::{Context, FieldResult, Object, SimpleObject};

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

#[derive(SimpleObject, Debug, Clone)]
pub struct Agreement {
    signature: String,
    data: String,
    protocol_network: String,
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
pub struct AgreementMutation {}

#[Object]
impl AgreementMutation {
    pub async fn create_agreement<'a>(
        &self,
        ctx: &'a Context<'_>,
        signature: String,
        data: String,
        protocol_network: String,
    ) -> FieldResult<Agreement> {
        let store: &Arc<dyn AgreementStore> = ctx.data()?;

        store
            .create_agreement(
                signature.clone(),
                Agreement {
                    signature,
                    data,
                    protocol_network,
                },
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
