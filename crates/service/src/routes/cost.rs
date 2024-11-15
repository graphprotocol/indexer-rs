// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thegraph_core::DeploymentId;

use crate::{
    database::cost_model::{self, CostModel}, metrics::{
        COST_MODEL_BATCH_FAILED, COST_MODEL_BATCH_INVALID, COST_MODEL_BATCH_METRIC,
        COST_MODEL_BATCH_SIZE, COST_MODEL_FAILED, COST_MODEL_INVALID, COST_MODEL_METRIC,
    }, service::SubgraphServiceState
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

#[derive(Default)]
pub struct Query;

#[Object]
impl Query {
    async fn cost_model(
        &self,
        ctx: &Context<'_>,
        deployment: String,
    ) -> Result<Option<GraphQlCostModel>, anyhow::Error> {
        let deployment_id =
            DeploymentId::from_str(&deployment).inspect_err(|_| COST_MODEL_INVALID.inc())?;

        let metric_deployment = deployment.clone();
        let _timer = COST_MODEL_METRIC
            .with_label_values(&[&metric_deployment])
            .start_timer();
        self._cost_model(ctx, deployment_id).await.inspect_err(|_| {
            COST_MODEL_FAILED
                .with_label_values(&[&metric_deployment])
                .inc()
        })
    }

    async fn cost_models(
        &self,
        ctx: &Context<'_>,
        deployments: Vec<String>,
    ) -> Result<Vec<GraphQlCostModel>, anyhow::Error> {
        let deployment_ids = deployments
            .into_iter()
            .map(|s| DeploymentId::from_str(&s))
            .collect::<Result<Vec<DeploymentId>, _>>()
            .inspect_err(|_| COST_MODEL_BATCH_INVALID.inc())?;

        let _metric = COST_MODEL_BATCH_METRIC.start_timer();

        COST_MODEL_BATCH_SIZE.observe(deployment_ids.len() as f64);

        self._cost_models(ctx, deployment_ids)
            .await
            .inspect_err(|_| COST_MODEL_BATCH_FAILED.inc())
    }
}

impl Query {
    async fn _cost_models(
        &self,
        ctx: &Context<'_>,
        deployment_ids: Vec<DeploymentId>,
    ) -> Result<Vec<GraphQlCostModel>, anyhow::Error> {
        let pool = &ctx.data_unchecked::<SubgraphServiceState>().database;
        let cost_models = cost_model::cost_models(pool, &deployment_ids).await?;
        Ok(cost_models.into_iter().map(|m| m.into()).collect())
    }

    async fn _cost_model(
        &self,
        ctx: &Context<'_>,
        deployment_id: DeploymentId,
    ) -> Result<Option<GraphQlCostModel>, anyhow::Error> {
        let pool = &ctx.data_unchecked::<SubgraphServiceState>().database;
        cost_model::cost_model(pool, &deployment_id)
            .await
            .map(|model_opt| model_opt.map(GraphQlCostModel::from))
    }
}

pub type CostSchema = Schema<Query, EmptyMutation, EmptySubscription>;

pub fn build_schema(data: SubgraphServiceState) -> CostSchema {
    Schema::build(Query, EmptyMutation, EmptySubscription)
        .data(data)
        .finish()
}
