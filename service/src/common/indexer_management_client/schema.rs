// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::{common::indexer_error::IndexerError, server::ServerOptions};

use super::{resolver, QueryRoot};

#[derive(Debug, FromRow, Clone, Serialize, Deserialize, SimpleObject)]
pub struct CostModel {
    pub deployment: String,
    pub model: Option<String>,
    pub variables: Option<Value>,
}

pub type CostSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

#[Object]
impl QueryRoot {
    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<CostModel>, IndexerError> {
        let pool = ctx
            .data_unchecked::<ServerOptions>()
            .indexer_management_client
            .database();

        let models: Vec<CostModel> = resolver::cost_models(pool, &deployments).await?;
        Ok(models)
    }

    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<CostModel>, IndexerError> {
        let pool = ctx
            .data_unchecked::<ServerOptions>()
            .indexer_management_client
            .database();

        let model = resolver::cost_model(pool, &deployment).await?;
        Ok(model)
    }
}
