// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use toolshed::thegraph::DeploymentId;

use crate::{
    common::indexer_management::{self, CostModel},
    server::ServerOptions,
};

#[derive(Clone, Debug, Serialize, Deserialize, SimpleObject)]
pub struct GraphQlCostModel {
    pub deployment: String,
    pub model: Option<String>,
    pub variables: Option<Value>,
}

impl From<CostModel> for GraphQlCostModel {
    fn from(model: CostModel) -> Self {
        Self {
            deployment: model.deployment.to_string(),
            model: model.model,
            variables: model.variables,
        }
    }
}

pub type CostSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

#[derive(Default)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<GraphQlCostModel>, anyhow::Error> {
        let deployment_ids = deployments
            .into_iter()
            .map(|s| DeploymentId::from_str(&s))
            .collect::<Result<Vec<DeploymentId>, _>>()?;
        let pool = &ctx.data_unchecked::<ServerOptions>().indexer_management_db;
        let cost_models = indexer_management::cost_models(pool, &deployment_ids).await?;
        Ok(cost_models.into_iter().map(|m| m.into()).collect())
    }

    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<GraphQlCostModel>, anyhow::Error> {
        let deployment_id = DeploymentId::from_str(&deployment)?;
        let pool = &ctx.data_unchecked::<ServerOptions>().indexer_management_db;
        indexer_management::cost_model(pool, &deployment_id)
            .await
            .map(|model_opt| model_opt.map(GraphQlCostModel::from))
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
