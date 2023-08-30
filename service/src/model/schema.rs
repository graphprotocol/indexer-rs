use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use sqlx::{Pool, Postgres};
use std::sync::Arc;
use thiserror::Error;

use crate::server::ServerOptions;

use super::resolver;

#[derive(Debug, FromRow, Clone, Serialize, Deserialize, SimpleObject)]
pub struct CostModel {
    pub deployment: String,
    pub model: Option<String>,
    pub variables: Option<Value>,
}

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
    ) -> Result<Vec<CostModel>, HttpServiceError> {
        let pool = ctx
            .data_unchecked::<ServerOptions>()
            .indexer_management_client
            .database();

        let models: Vec<CostModel> = resolver::cost_models(pool, deployments).await?;
        Ok(models)
    }

    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<CostModel>, HttpServiceError> {
        let pool = ctx
            .data_unchecked::<ServerOptions>()
            .indexer_management_client
            .database();

        let model = resolver::cost_model(pool, &deployment).await?;
        Ok(model)
    }
}

#[derive(Error, Debug)]
pub enum HttpServiceError {
    #[error("HTTP client error: {0}")]
    HttpClientError(#[from] reqwest::Error),
    #[error("{0}")]
    Others(#[from] anyhow::Error),
}
