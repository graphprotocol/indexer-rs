// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{str::FromStr, sync::LazyLock};

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use prometheus::{
    register_counter, register_counter_vec, register_histogram, register_histogram_vec, Counter,
    CounterVec, Histogram, HistogramVec,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use thegraph_core::DeploymentId;

use crate::database::cost_model::{self, CostModel};

pub static COST_MODEL_METRIC: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "indexer_cost_model_seconds",
        "Histogram metric for single cost model query",
        &["deployment"]
    )
    .unwrap()
});
pub static COST_MODEL_FAILED: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "indexer_cost_model_failed_total",
        "Total failed Cost Model query",
        &["deployment"]
    )
    .unwrap()
});
pub static COST_MODEL_INVALID: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!(
        "indexer_cost_model_invalid_total",
        "Cost model queries with invalid deployment id",
    )
    .unwrap()
});
pub static COST_MODEL_BATCH_METRIC: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "indexer_cost_model_batch_seconds",
        "Histogram metric for batch cost model query",
    )
    .unwrap()
});
pub static COST_MODEL_BATCH_SIZE: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "indexer_cost_model_batch_size",
        "This shows the size of deployment ids cost model batch queries got",
    )
    .unwrap()
});
pub static COST_MODEL_BATCH_FAILED: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!(
        "indexer_cost_model_batch_failed_total",
        "Total failed batch cost model queries",
    )
    .unwrap()
});
pub static COST_MODEL_BATCH_INVALID: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!(
        "indexer_cost_model_batch_invalid_total",
        "Batch cost model queries with invalid deployment ids",
    )
    .unwrap()
});
pub static COST_MODEL_BATCH_LIMIT_EXCEEDED: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!(
        "indexer_cost_model_batch_limit_exceeded_total",
        "Batch cost model queries rejected for exceeding size limit",
    )
    .unwrap()
});

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

pub struct Query {
    max_batch_size: usize,
}

impl Query {
    pub fn new(max_batch_size: usize) -> Self {
        Self { max_batch_size }
    }
}

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
        if deployments.len() > self.max_batch_size {
            COST_MODEL_BATCH_LIMIT_EXCEEDED.inc();
            return Err(anyhow::anyhow!(
                "Batch size {} exceeds maximum allowed ({})",
                deployments.len(),
                self.max_batch_size
            ));
        }

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
        let pool = &ctx.data_unchecked::<PgPool>();
        let cost_models = cost_model::cost_models(pool, &deployment_ids).await?;
        Ok(cost_models.into_iter().map(|m| m.into()).collect())
    }

    async fn _cost_model(
        &self,
        ctx: &Context<'_>,
        deployment_id: DeploymentId,
    ) -> Result<Option<GraphQlCostModel>, anyhow::Error> {
        let pool = &ctx.data_unchecked::<PgPool>();
        Ok(cost_model::cost_model(pool, &deployment_id)
            .await
            .map(|model_opt| model_opt.map(GraphQlCostModel::from))?)
    }
}

pub type CostSchema = Schema<Query, EmptyMutation, EmptySubscription>;

pub async fn build_schema(data: PgPool, max_batch_size: usize) -> CostSchema {
    Schema::build(Query::new(max_batch_size), EmptyMutation, EmptySubscription)
        .data(data)
        .finish()
}
