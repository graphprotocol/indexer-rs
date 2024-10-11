// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use bigdecimal::ToPrimitive;
use cost_model::CostModel;
use sqlx::{postgres::PgListener, PgPool};
use std::{
    cmp::min,
    collections::{hash_map::Entry, HashMap, VecDeque},
    str::FromStr,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use thegraph_core::{DeploymentId, ParseDeploymentIdError};
use tokio::{sync::mpsc::channel, task::JoinHandle, time::sleep};
use tracing::error;

use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    Context, ReceiptWithState,
};

// we only accept receipts with minimal 1 wei grt
const MINIMAL_VALUE: u128 = 1;

type CostModelMap = Arc<RwLock<HashMap<DeploymentId, RwLock<CostModelCache>>>>;

pub struct MinimumValue {
    cost_model_cache: CostModelMap,
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
        let cost_model_cache: CostModelMap = Default::default();

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
        let models = cache.get(&agora_query.deployment_id);

        // TODO check global cost model

        let expected_value = models
            .map(|cache| {
                let cache = cache.read().unwrap();
                cache.cost(agora_query)
            })
            .unwrap_or(MINIMAL_VALUE);

        Ok(expected_value)
    }

    async fn cost_models_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        cost_model_cache: CostModelMap,
        cancel_token: tokio_util::sync::CancellationToken,
    ) {
        let handles: Arc<Mutex<HashMap<DeploymentId, VecDeque<JoinHandle<()>>>>> =
            Default::default();
        let (tx, mut rx) = channel::<DeploymentId>(64);

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }
                Some(deployment_id) = rx.recv() => {
                    let mut cost_model_write = cost_model_cache.write().unwrap();
                    if let Some(cache) = cost_model_write.get_mut(&deployment_id) {
                        cache.get_mut().unwrap().expire();
                    }

                    if let Entry::Occupied(mut entry) = handles.lock().unwrap().entry(deployment_id) {
                        let vec = entry.get_mut();
                        vec.pop_front();
                        if vec.is_empty() {
                            entry.remove();
                        }
                    }

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
                            {
                                let mut cost_model_write = cost_model_cache
                                    .write()
                                    .unwrap();
                                let cache = cost_model_write.entry(deployment_id).or_default();
                                let _ = cache.get_mut().unwrap().insert_model(cost_model_source);
                            }
                            let _tx = tx.clone();

                            // expire after 60 seconds
                            handles.lock()
                                .unwrap()
                                .entry(deployment_id)
                                .or_default()
                                .push_back(tokio::spawn(async move {
                                // 1 minute after, we expire the older cache
                                sleep(Duration::from_secs(60)).await;
                                let _ = _tx.send(deployment_id).await;
                            }));
                        }
                        "DELETE" => {
                            if let Entry::Occupied(mut entry) = cost_model_cache
                                        .write().unwrap().entry(cost_model_notification.deployment) {
                                let should_remove = {
                                    let mut cost_model = entry.get_mut().write().unwrap();
                                    cost_model.expire();
                                    cost_model.is_empty()
                                };
                                if should_remove {
                                    entry.remove();
                                }
                            }
                        }
                        // UPDATE and TRUNCATE are not expected to happen. Reload the entire cost
                        // model cache.
                        _ => {
                            error!(
                                "Received an unexpected cost model table notification: {}. Reloading entire \
                                cost model.",
                                cost_model_notification.tg_op
                            );

                            {
                                // clear all pending expire
                                let mut handles = handles.lock().unwrap();
                                for maps in handles.values() {
                                    for handle in maps {
                                        handle.abort();
                                    }
                                }
                                handles.clear();
                            }

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
        cost_model_cache: CostModelMap,
    ) -> anyhow::Result<()> {
        // TODO make sure to load last cost model
        // plus all models that were created 60 secoonds from now
        //
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
                let mut model = CostModelCache::default();
                let _ = model.insert_model(CostModelSource {
                    deployment_id,
                    model: record.model.unwrap(),
                    variables: record.variables.unwrap_or_default(),
                });

                Ok::<_, ParseDeploymentIdError>((deployment_id, RwLock::new(model)))
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
    let model = CostModel::compile(&src.model, &src.variables.to_string())?;
    Ok(model)
}

pub struct AgoraQuery {
    pub deployment_id: DeploymentId,
    pub query: String,
    pub variables: String,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct CostModelSource {
    pub deployment_id: DeploymentId,
    pub model: String,
    pub variables: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct CostModelNotification {
    tg_op: String,
    deployment: DeploymentId,
    model: String,
    variables: serde_json::Value,
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

#[derive(Default)]
struct CostModelCache {
    models: VecDeque<CostModel>,
}

impl CostModelCache {
    fn insert_model(&mut self, source: CostModelSource) -> anyhow::Result<()> {
        let model = compile_cost_model(source.clone())?;
        self.models.push_back(model);
        Ok(())
    }

    fn expire(&mut self) {
        self.models.pop_front();
    }

    fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    fn cost(&self, query: &AgoraQuery) -> u128 {
        self.models
            .iter()
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
            .unwrap_or(MINIMAL_VALUE)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    #[sqlx::test(migrations = "../migrations")]
    async fn initialize_check(_pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_initialize_check_with_caches(_pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_add_model_to_cache_on_insert(_pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_expire_old_model(_pg_pool: PgPool) {}

    #[sqlx::test(migrations = "../migrations")]
    async fn should_verify_global_model(_pg_pool: PgPool) {}
}
