// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::{
    common::indexer_error::{IndexerError, IndexerErrorCause, IndexerErrorCode},
    server::ServerOptions,
};

use super::resolver;

#[derive(Debug, FromRow, Clone, Serialize, Deserialize, SimpleObject)]
pub struct CostModel {
    pub deployment: String,
    pub model: Option<String>,
    pub variables: Option<Value>,
}

unsafe impl Send for CostModel {}

pub type CostSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

// Unified query object for resolvers
#[derive(Default)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    // List messages but without filter options since msg fields are saved in jsonb
    // Later flatten the messages to have columns from graphcast message.
    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<CostModel>, IndexerError> {
        let pool = ctx
            .data_unchecked::<ServerOptions>()
            .indexer_management_client
            .database();

        let models: Vec<CostModel> =
            resolver::cost_models(pool, &deployments)
                .await
                .map_err(|e| {
                    IndexerError::new(IndexerErrorCode::IE025, Some(IndexerErrorCause::new(e)))
                })?;
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

        let model = resolver::cost_model(pool, &deployment).await.map_err(|e| {
            IndexerError::new(IndexerErrorCode::IE025, Some(IndexerErrorCause::new(e)))
        })?;
        Ok(model)
    }
}
