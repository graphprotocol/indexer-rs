// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::sync::Arc;

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use indexer_common::tap::CostModelSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thegraph::types::DeploymentId;

use crate::database::{self, CostModel};
use crate::SubgraphServiceState;

#[derive(Clone, Debug, Serialize, Deserialize, SimpleObject)]
pub struct GraphQlCostModel {
    pub deployment: String,
    pub model: Option<String>,
    pub variables: Option<Value>,
}

impl From<CostModel> for CostModelSource {
    fn from(value: CostModel) -> Self {
        Self {
            deployment_id: value.deployment,
            model: value.model.unwrap_or_default(),
            variables: value.variables.unwrap_or_default().to_string(),
        }
    }
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

#[derive(Default)]
pub struct Query;

#[Object]
impl Query {
    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<GraphQlCostModel>, anyhow::Error> {
        let deployment_ids = deployments
            .into_iter()
            .map(|s| DeploymentId::from_str(&s))
            .collect::<Result<Vec<DeploymentId>, _>>()?;
        let state = &ctx.data_unchecked::<Arc<SubgraphServiceState>>();

        let cost_model_sender = &state.value_check_sender;

        let pool = &state.database;
        let cost_models = database::cost_models(pool, &deployment_ids).await?;

        for model in &cost_models {
            let _ = cost_model_sender
                .tx_cost_model
                .send(CostModelSource::from(model.clone()))
                .await;
        }

        Ok(cost_models.into_iter().map(|m| m.into()).collect())
    }

    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<GraphQlCostModel>, anyhow::Error> {
        let deployment_id = DeploymentId::from_str(&deployment)?;

        let state = &ctx.data_unchecked::<Arc<SubgraphServiceState>>();
        let cost_model_sender = &state.value_check_sender;
        let pool = &ctx.data_unchecked::<Arc<SubgraphServiceState>>().database;
        let model = database::cost_model(pool, &deployment_id).await?;

        if let Some(model) = &model {
            let _ = cost_model_sender
                .tx_cost_model
                .send(CostModelSource::from(model.clone()))
                .await;
        }

        Ok(model.map(GraphQlCostModel::from))
    }
}

pub type CostSchema = Schema<Query, EmptyMutation, EmptySubscription>;

pub async fn build_schema() -> CostSchema {
    Schema::build(Query, EmptyMutation, EmptySubscription).finish()
}

pub async fn cost(
    State(state): State<Arc<SubgraphServiceState>>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    state
        .cost_schema
        .execute(req.into_inner().data(state.clone()))
        .await
        .into()
}
