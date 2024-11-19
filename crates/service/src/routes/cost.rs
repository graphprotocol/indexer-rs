// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use crate::database::cost_model::{self, CostModel};
use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use lazy_static::lazy_static;
use prometheus::{
    register_counter, register_counter_vec, register_histogram, register_histogram_vec, Counter,
    CounterVec, Histogram, HistogramVec,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thegraph_core::DeploymentId;

use crate::service::SubgraphServiceState;

lazy_static! {
    pub static ref COST_MODEL_METRIC: HistogramVec = register_histogram_vec!(
        "indexer_cost_model_seconds",
        "Histogram metric for single cost model query",
        &["deployment"]
    )
    .unwrap();
    pub static ref COST_MODEL_FAILED: CounterVec = register_counter_vec!(
        "indexer_cost_model_failed_total",
        "Total failed Cost Model query",
        &["deployment"]
    )
    .unwrap();
    pub static ref COST_MODEL_INVALID: Counter = register_counter!(
        "indexer_cost_model_invalid_total",
        "Cost model queries with invalid deployment id",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_METRIC: Histogram = register_histogram!(
        "indexer_cost_model_batch_seconds",
        "Histogram metric for batch cost model query",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_SIZE: Histogram = register_histogram!(
        "indexer_cost_model_batch_size",
        "This shows the size of deployment ids cost model batch queries got",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_FAILED: Counter = register_counter!(
        "indexer_cost_model_batch_failed_total",
        "Total failed batch cost model queries",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_INVALID: Counter = register_counter!(
        "indexer_cost_model_batch_invalid_total",
        "Batch cost model queries with invalid deployment ids",
    )
    .unwrap();
}

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

pub async fn build_schema() -> CostSchema {
    Schema::build(Query, EmptyMutation, EmptySubscription).finish()
}

pub async fn cost(
    State(state): State<SubgraphServiceState>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    state
        .cost_schema
        .execute(req.into_inner().data(state.clone()))
        .await
        .into()
}
