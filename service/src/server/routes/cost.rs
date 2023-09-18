// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;

use crate::{
    common::{
        indexer_error::IndexerError,
        indexer_management::{self, CostModel},
    },
    server::ServerOptions,
};

pub type CostSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

#[derive(Default)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<CostModel>, IndexerError> {
        let pool = &ctx.data_unchecked::<ServerOptions>().indexer_management_db;
        indexer_management::cost_models(pool, &deployments).await
    }

    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<CostModel>, IndexerError> {
        let pool = &ctx.data_unchecked::<ServerOptions>().indexer_management_db;
        indexer_management::cost_model(pool, &deployment).await
    }
}

pub(crate) async fn graphql_handler(
    req: GraphQLRequest,
    Extension(schema): Extension<CostSchema>,
    Extension(server_options): Extension<ServerOptions>,
) -> GraphQLResponse {
    schema
        .execute(req.into_inner().data(server_options))
        .await
        .into()
}
