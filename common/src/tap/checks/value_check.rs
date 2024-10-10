// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use bigdecimal::ToPrimitive;
use cost_model::CostModel;
use sqlx::{postgres::PgListener, PgPool};
use std::{
    cmp::min,
    collections::HashMap,
    str::FromStr,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use thegraph_core::DeploymentId;
use tracing::error;
use ttl_cache::TtlCache;

use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    Context, ReceiptWithState,
};

pub struct MinimumValue {
    cost_model_cache: Arc<RwLock<HashMap<DeploymentId, Mutex<CostModelCache>>>>,
    watcher_cancel_token: tokio_util::sync::CancellationToken,
}

impl Drop for MinimumValue {
    fn drop(&mut self) {
        // Clean shutdown for the sender_denylist_watcher
        // Though since it's not a critical task, we don't wait for it to finish (join).
        self.watcher_cancel_token.cancel();
    }
}

impl MinimumValue {
    pub async fn new(pgpool: PgPool) -> Self {
        let cost_model_cache = Arc::new(RwLock::new(
            HashMap::<DeploymentId, Mutex<CostModelCache>>::new(),
        ));

        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener.listen("cost_models_update_notify").await.expect(
            "should be able to subscribe to Postgres Notify events on the channel \
                'cost_models_update_notify'",
        );

        let watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(Self::cost_models_watcher(
            pgpool.clone(),
            pglistener,
            cost_model_cache.clone(),
            watcher_cancel_token.clone(),
        ));

        Self {
            cost_model_cache,
            watcher_cancel_token,
        }
    }

    fn get_expected_value(&self, agora_query: &AgoraQuery) -> anyhow::Result<u128> {
        // get agora model for the allocation_id
        let cache = self.cost_model_cache.read().unwrap();
        // on average, we'll have zero or one model
        let models = cache.get(&agora_query.deployment_id);

        let expected_value = models
            .map(|cache| cache.lock().unwrap().cost(agora_query))
            .unwrap_or_default();

        Ok(expected_value)
    }

    async fn cost_models_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        cost_model_cache: Arc<RwLock<HashMap<DeploymentId, Mutex<CostModelCache>>>>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }

                pg_notification = pglistener.recv() => {
                    let pg_notification = pg_notification.expect(
                    "should be able to receive Postgres Notify events on the channel \
                    'cost_models_update_notify'",
                    );

                    let cost_model_notification: CostModelNotification =
                        serde_json::from_str(pg_notification.payload()).expect(
                            "should be able to deserialize the Postgres Notify event payload as a \
                            CostModelNotification",
                        );

                    let deployment_id = cost_model_notification.deployment;

                    match cost_model_notification.tg_op.as_str() {
                        "INSERT" => {
                            let cost_model_source: CostModelSource = cost_model_notification.into();
                            let mut cost_model_cache = cost_model_cache
                                .write()
                                .unwrap();

                            match cost_model_cache.get_mut(&deployment_id) {
                                Some(cache) => {
                                    let _ = cache.lock().unwrap().insert_model(cost_model_source);
                                },
                                None => {
                                     if let Ok(cache) = CostModelCache::new(cost_model_source).inspect_err(|err| {
                                        tracing::error!(
                                            "Error while compiling cost model for deployment id {}. Error: {}",
                                            deployment_id, err
                                        )
                                    }) {
                                        cost_model_cache.insert(deployment_id, Mutex::new(cache));
                                    }
                                },
                            }
                        }
                        "DELETE" => {
                            cost_model_cache
                                .write()
                                .unwrap()
                                .remove(&cost_model_notification.deployment);
                        }
                        // UPDATE and TRUNCATE are not expected to happen. Reload the entire cost
                        // model cache.
                        _ => {
                            error!(
                                "Received an unexpected cost model table notification: {}. Reloading entire \
                                cost model.",
                                cost_model_notification.tg_op
                            );

                            Self::value_check_reload(&pgpool, cost_model_cache.clone())
                                .await
                                .expect("should be able to reload cost models")
                        }
                    }
                }
            }
        }
    }

    async fn value_check_reload(
        pgpool: &PgPool,
        cost_model_cache: Arc<RwLock<HashMap<DeploymentId, Mutex<CostModelCache>>>>,
    ) -> anyhow::Result<()> {
        let models = sqlx::query!(
            r#"
            SELECT deployment, model, variables
            FROM "CostModels"
            WHERE deployment != 'global'
            ORDER BY deployment ASC
            "#
        )
        .fetch_all(pgpool)
        .await?;
        let models = models
            .into_iter()
            .map(|record| {
                let deployment_id = DeploymentId::from_str(&record.deployment.unwrap())?;
                let model = CostModelCache::new(CostModelSource {
                    deployment_id,
                    model: record.model.unwrap(),
                    variables: record.variables.unwrap().to_string(),
                })?;

                Ok::<_, anyhow::Error>((deployment_id, Mutex::new(model)))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;

        *(cost_model_cache.write().unwrap()) = models;

        Ok(())
    }
}

#[async_trait::async_trait]
impl Check for MinimumValue {
    async fn check(&self, ctx: &Context, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let agora_query = ctx
            .get()
            .ok_or(CheckError::Failed(anyhow!("Could not find agora query")))?;

        let expected_value = self
            .get_expected_value(agora_query)
            .map_err(CheckError::Failed)?;

        // get value
        let value = receipt.signed_receipt().message.value;

        let should_accept = value >= expected_value;

        tracing::trace!(
            value,
            expected_value,
            should_accept,
            "Evaluating mininum query fee."
        );

        if should_accept {
            Ok(())
        } else {
            return Err(CheckError::Failed(anyhow!(
                "Query receipt does not have the minimum value. Expected value: {}. Received value: {}.",
                expected_value, value,
            )));
        }
    }
}

fn compile_cost_model(src: CostModelSource) -> anyhow::Result<CostModel> {
    if src.model.len() > (1 << 16) {
        return Err(anyhow!("CostModelTooLarge"));
    }
    let model = CostModel::compile(&src.model, &src.variables)?;
    Ok(model)
}

pub struct AgoraQuery {
    pub deployment_id: DeploymentId,
    pub query: String,
    pub variables: String,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CostModelSource {
    pub deployment_id: DeploymentId,
    pub model: String,
    pub variables: String,
}

#[derive(serde::Deserialize)]
struct CostModelNotification {
    tg_op: String,
    deployment: DeploymentId,
    model: String,
    variables: String,
}

impl From<CostModelNotification> for CostModelSource {
    fn from(value: CostModelNotification) -> Self {
        CostModelSource {
            deployment_id: value.deployment,
            model: value.model,
            variables: value.variables,
        }
    }
}

pub struct CostModelCache {
    models: TtlCache<CostModelSource, CostModel>,
    latest_model: CostModel,
    latest_source: CostModelSource,
}

impl CostModelCache {
    pub fn new(source: CostModelSource) -> anyhow::Result<Self> {
        let model = compile_cost_model(source.clone())?;
        Ok(Self {
            latest_model: model,
            latest_source: source,
            // arbitrary number of models copy
            models: TtlCache::new(10),
        })
    }

    fn insert_model(&mut self, source: CostModelSource) -> anyhow::Result<()> {
        if source != self.latest_source {
            let model = compile_cost_model(source.clone())?;
            // update latest and insert into ttl the old model
            let old_model = std::mem::replace(&mut self.latest_model, model);
            self.latest_source = source.clone();

            self.models
                // arbitrary cache duration
                .insert(source, old_model, Duration::from_secs(60));
        }
        Ok(())
    }

    fn get_models(&mut self) -> Vec<&CostModel> {
        let mut values: Vec<&CostModel> = self.models.iter().map(|(_, v)| v).collect();
        values.push(&self.latest_model);
        values
    }

    fn cost(&mut self, query: &AgoraQuery) -> u128 {
        let models = self.get_models();

        models
            .into_iter()
            .fold(None, |acc, model| {
                let value = model
                    .cost(&query.query, &query.variables)
                    .ok()
                    .map(|fee| fee.to_u128().unwrap_or_default())
                    .unwrap_or_default();
                if let Some(acc) = acc {
                    // return the minimum value of the cache list
                    Some(min(acc, value))
                } else {
                    Some(value)
                }
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    #[sqlx::test(migrations = "../migrations")]
    async fn initialize_check(pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_initialize_check_with_caches(pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_add_model_to_cache_on_insert(pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_expire_old_model(pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_verify_global_model(pg_pool: PgPool) {}
}
