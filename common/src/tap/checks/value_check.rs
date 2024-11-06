// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use ::cost_model::CostModel;
use anyhow::anyhow;
use bigdecimal::ToPrimitive;
use sqlx::{
    postgres::{PgListener, PgNotification},
    PgPool,
};
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Instant,
};
use thegraph_core::DeploymentId;
use tracing::error;

use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    state::Checking,
    Context, ReceiptWithState,
};

use crate::cost_model;

// we only accept receipts with minimal 1 wei grt
const MINIMAL_VALUE: u128 = 1;

/// Represents a query that can be checked against an agora model
///
/// It contains the deployment_id to check which agora model evaluate
/// and also the query and variables to perform the evaluation
pub struct AgoraQuery {
    pub deployment_id: DeploymentId,
    pub query: String,
    pub variables: String,
}

type CostModelMap = Arc<RwLock<HashMap<DeploymentId, CostModel>>>;
type GlobalModel = Arc<RwLock<Option<CostModel>>>;
type GracePeriod = Arc<RwLock<Instant>>;

/// Represents the check for minimum for a receipt
///
/// It contains all information needed in memory to
/// make it as fast as possible
pub struct MinimumValue {
    cost_model_map: CostModelMap,
    global_model: GlobalModel,
    watcher_cancel_token: tokio_util::sync::CancellationToken,
    updated_at: GracePeriod,
    grace_period: u64,
}

struct CostModelWatcher {
    pgpool: PgPool,

    cost_models: CostModelMap,
    global_model: GlobalModel,
    updated_at: GracePeriod,
}

impl CostModelWatcher {
    async fn cost_models_watcher(
        pgpool: PgPool,
        mut pglistener: PgListener,
        cost_models: CostModelMap,
        global_model: GlobalModel,
        cancel_token: tokio_util::sync::CancellationToken,
        grace_period: GracePeriod,
    ) {
        let cost_model_watcher = CostModelWatcher {
            pgpool,
            global_model,
            cost_models,
            updated_at: grace_period,
        };

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }
                Ok(pg_notification) = pglistener.recv() => {
                    cost_model_watcher.new_notification(
                        pg_notification,
                    ).await;
                }
            }
        }
    }

    async fn new_notification(&self, pg_notification: PgNotification) {
        let payload = pg_notification.payload();
        let cost_model_notification: Result<CostModelNotification, _> =
            serde_json::from_str(payload);

        match cost_model_notification {
            Ok(CostModelNotification::Insert {
                deployment,
                model,
                variables,
            }) => self.handle_insert(deployment, model, variables),
            Ok(CostModelNotification::Delete { deployment }) => self.handle_delete(deployment),
            // UPDATE and TRUNCATE are not expected to happen. Reload the entire cost
            // model cache.
            Err(_) => self.handle_unexpected_notification(payload).await,
        }
    }

    fn handle_insert(&self, deployment: String, model: String, variables: String) {
        let model = compile_cost_model(model, variables).unwrap();

        match deployment.as_str() {
            "global" => {
                *self.global_model.write().unwrap() = Some(model);
            }
            deployment_id => match DeploymentId::from_str(deployment_id) {
                Ok(deployment_id) => {
                    let mut cost_model_write = self.cost_models.write().unwrap();
                    cost_model_write.insert(deployment_id, model);
                }
                Err(_) => {
                    error!(
                        "Received insert request for an invalid deployment_id: {}",
                        deployment_id
                    )
                }
            },
        };

        *self.updated_at.write().unwrap() = Instant::now();
    }

    fn handle_delete(&self, deployment: String) {
        match deployment.as_str() {
            "global" => {
                *self.global_model.write().unwrap() = None;
            }
            deployment_id => match DeploymentId::from_str(deployment_id) {
                Ok(deployment_id) => {
                    self.cost_models.write().unwrap().remove(&deployment_id);
                }
                Err(_) => {
                    error!(
                        "Received delete request for an invalid deployment_id: {}",
                        deployment_id
                    )
                }
            },
        };
        *self.updated_at.write().unwrap() = Instant::now();
    }

    async fn handle_unexpected_notification(&self, payload: &str) {
        error!(
            "Received an unexpected cost model table notification: {}. Reloading entire \
                                cost model.",
            payload
        );

        MinimumValue::value_check_reload(
            &self.pgpool,
            self.cost_models.clone(),
            self.global_model.clone(),
        )
        .await
        .expect("should be able to reload cost models");

        *self.updated_at.write().unwrap() = Instant::now();
    }
}

impl Drop for MinimumValue {
    fn drop(&mut self) {
        // Clean shutdown for the minimum value check
        // Though since it's not a critical task, we don't wait for it to finish (join).
        self.watcher_cancel_token.cancel();
    }
}

impl MinimumValue {
    pub async fn new(pgpool: PgPool, grace_period: u64) -> Self {
        let cost_model_map: CostModelMap = Default::default();
        let global_model: GlobalModel = Default::default();
        let updated_at: GracePeriod = Arc::new(RwLock::new(Instant::now()));
        Self::value_check_reload(&pgpool, cost_model_map.clone(), global_model.clone())
            .await
            .expect("should be able to reload cost models");

        let mut pglistener = PgListener::connect_with(&pgpool.clone()).await.unwrap();
        pglistener
            .listen("cost_models_update_notification")
            .await
            .expect(
                "should be able to subscribe to Postgres Notify events on the channel \
                'cost_models_update_notification'",
            );

        let watcher_cancel_token = tokio_util::sync::CancellationToken::new();
        tokio::spawn(CostModelWatcher::cost_models_watcher(
            pgpool.clone(),
            pglistener,
            cost_model_map.clone(),
            global_model.clone(),
            watcher_cancel_token.clone(),
            updated_at.clone(),
        ));
        Self {
            global_model,
            cost_model_map,
            watcher_cancel_token,
            updated_at,
            grace_period,
        }
    }

    fn inside_grace_period(&self) -> bool {
        let time_elapsed = Instant::now().duration_since(*self.updated_at.read().unwrap());
        time_elapsed.as_secs() < self.grace_period
    }

    fn expected_value(&self, agora_query: &AgoraQuery) -> anyhow::Result<u128> {
        // get agora model for the deployment_id
        let model = self.cost_model_map.read().unwrap();
        let subgraph_model = model.get(&agora_query.deployment_id);
        let global_model = self.global_model.read().unwrap();

        let expected_value = match (subgraph_model, global_model.as_ref()) {
            (Some(model), _) | (_, Some(model)) => model
                .cost(&agora_query.query, &agora_query.variables)
                .map(|fee| fee.to_u128())
                .ok()
                .flatten(),
            _ => None,
        };

        Ok(expected_value.unwrap_or(MINIMAL_VALUE))
    }

    async fn value_check_reload(
        pgpool: &PgPool,
        cost_model_map: CostModelMap,
        global_model: GlobalModel,
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
            .flat_map(|record| {
                let deployment_id = DeploymentId::from_str(&record.deployment).ok()?;
                let model = compile_cost_model(
                    record.model?,
                    record.variables.map(|v| v.to_string()).unwrap_or_default(),
                )
                .ok()?;
                Some((deployment_id, model))
            })
            .collect::<HashMap<_, _>>();

        *cost_model_map.write().unwrap() = models;

        *global_model.write().unwrap() =
            cost_model::global_cost_model(pgpool)
                .await?
                .and_then(|model| {
                    compile_cost_model(
                        model.model.unwrap_or_default(),
                        model.variables.map(|v| v.to_string()).unwrap_or_default(),
                    )
                    .ok()
                });

        Ok(())
    }
}

#[async_trait::async_trait]
impl Check for MinimumValue {
    async fn check(&self, ctx: &Context, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let agora_query = ctx
            .get()
            .ok_or(CheckError::Failed(anyhow!("Could not find agora query")))?;
        // get value
        let value = receipt.signed_receipt().message.value;

        if self.inside_grace_period() && value > MINIMAL_VALUE {
            return Ok(());
        }

        let expected_value = self
            .expected_value(agora_query)
            .map_err(CheckError::Failed)?;

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

fn compile_cost_model(model: String, variables: String) -> anyhow::Result<CostModel> {
    if model.len() > (1 << 16) {
        return Err(anyhow!("CostModelTooLarge"));
    }
    let model = CostModel::compile(&model, &variables)?;
    Ok(model)
}

#[derive(serde::Deserialize)]
#[serde(tag = "tg_op")]
enum CostModelNotification {
    #[serde(rename = "INSERT")]
    Insert {
        deployment: String,
        model: String,
        variables: String,
    },
    #[serde(rename = "DELETE")]
    Delete { deployment: String },
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{address, Address};
    use std::time::Duration;

    use sqlx::PgPool;
    use tap_core::receipt::{checks::Check, Context, ReceiptWithState};
    use tokio::time::sleep;

    use crate::{
        cost_model::test::{add_cost_models, global_cost_model, to_db_models},
        tap::AgoraQuery,
        test_vectors::create_signed_receipt,
    };

    use super::MinimumValue;

    #[sqlx::test(migrations = "../migrations")]
    async fn initialize_check(pgpool: PgPool) {
        let check = MinimumValue::new(pgpool, 0).await;
        assert_eq!(check.cost_model_map.read().unwrap().len(), 0);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_initialize_check_with_models(pgpool: PgPool) {
        // insert 2 cost models for different deployment_id
        let test_models = crate::cost_model::test::test_data();

        add_cost_models(&pgpool, to_db_models(test_models.clone())).await;

        let check = MinimumValue::new(pgpool, 0).await;
        assert_eq!(check.cost_model_map.read().unwrap().len(), 2);

        // no global model
        assert!(check.global_model.read().unwrap().is_none());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_watch_model_insert(pgpool: PgPool) {
        let check = MinimumValue::new(pgpool.clone(), 0).await;
        assert_eq!(check.cost_model_map.read().unwrap().len(), 0);

        // insert 2 cost models for different deployment_id
        let test_models = crate::cost_model::test::test_data();
        add_cost_models(&pgpool, to_db_models(test_models.clone())).await;
        sleep(Duration::from_millis(200)).await;

        assert_eq!(
            check.cost_model_map.read().unwrap().len(),
            test_models.len()
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_watch_model_remove(pgpool: PgPool) {
        // insert 2 cost models for different deployment_id
        let test_models = crate::cost_model::test::test_data();
        add_cost_models(&pgpool, to_db_models(test_models.clone())).await;

        let check = MinimumValue::new(pgpool.clone(), 0).await;
        assert_eq!(check.cost_model_map.read().unwrap().len(), 2);

        // remove
        sqlx::query!(r#"DELETE FROM "CostModels""#)
            .execute(&pgpool)
            .await
            .unwrap();

        sleep(Duration::from_millis(200)).await;

        assert_eq!(check.cost_model_map.read().unwrap().len(), 0);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_start_global_model(pgpool: PgPool) {
        let global_model = global_cost_model();
        add_cost_models(&pgpool, vec![global_model.clone()]).await;

        let check = MinimumValue::new(pgpool.clone(), 0).await;
        assert!(check.global_model.read().unwrap().is_some());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_watch_global_model(pgpool: PgPool) {
        let check = MinimumValue::new(pgpool.clone(), 0).await;

        let global_model = global_cost_model();
        add_cost_models(&pgpool, vec![global_model.clone()]).await;
        sleep(Duration::from_millis(10)).await;

        assert!(check.global_model.read().unwrap().is_some());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_remove_global_model(pgpool: PgPool) {
        let global_model = global_cost_model();
        add_cost_models(&pgpool, vec![global_model.clone()]).await;

        let check = MinimumValue::new(pgpool.clone(), 0).await;
        assert!(check.global_model.read().unwrap().is_some());

        sqlx::query!(r#"DELETE FROM "CostModels""#)
            .execute(&pgpool)
            .await
            .unwrap();

        sleep(Duration::from_millis(10)).await;

        assert_eq!(check.cost_model_map.read().unwrap().len(), 0);
    }

    const ALLOCATION_ID: Address = address!("deadbeefcafebabedeadbeefcafebabedeadbeef");

    #[sqlx::test(migrations = "../migrations")]
    async fn should_check_minimal_value(pgpool: PgPool) {
        // insert cost models for different deployment_id
        let test_models = crate::cost_model::test::test_data();

        add_cost_models(&pgpool, to_db_models(test_models.clone())).await;

        let check = MinimumValue::new(pgpool, 1).await;

        let deployment_id = test_models[0].deployment;
        let mut ctx = Context::new();
        ctx.insert(AgoraQuery {
            deployment_id,
            query: "query { a(skip: 10), b(bob: 5) }".into(),
            variables: "".into(),
        });

        let signed_receipt = create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, 0).await;
        let receipt = ReceiptWithState::new(signed_receipt);

        assert!(
            check.check(&ctx, &receipt).await.is_err(),
            "Should deny if value is 0 for any query"
        );

        let signed_receipt = create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, 1).await;
        let receipt = ReceiptWithState::new(signed_receipt);
        assert!(
            check.check(&ctx, &receipt).await.is_ok(),
            "Should accept if value is more than 0 for any query"
        );

        let deployment_id = test_models[1].deployment;
        let mut ctx = Context::new();
        ctx.insert(AgoraQuery {
            deployment_id,
            query: "query { a(skip: 10), b(bob: 5) }".into(),
            variables: "".into(),
        });
        let minimal_value = 500000000000000;

        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, minimal_value - 1).await;
        let receipt = ReceiptWithState::new(signed_receipt);

        assert!(
            check.check(&ctx, &receipt).await.is_ok(),
            "Should accept since its inside grace period "
        );

        sleep(Duration::from_millis(1010)).await;

        assert!(
            check.check(&ctx, &receipt).await.is_ok(),
            "Should require minimal value"
        );

        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, 500000000000000).await;
        let receipt = ReceiptWithState::new(signed_receipt);
        check
            .check(&ctx, &receipt)
            .await
            .expect("should accept equals minimal");

        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, minimal_value + 1).await;
        let receipt = ReceiptWithState::new(signed_receipt);
        check
            .check(&ctx, &receipt)
            .await
            .expect("should accept more than minimal");
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn should_check_using_global(pgpool: PgPool) {
        // insert cost models for different deployment_id
        let test_models = crate::cost_model::test::test_data();
        let global_model = global_cost_model();

        add_cost_models(&pgpool, vec![global_model.clone()]).await;
        add_cost_models(&pgpool, to_db_models(test_models.clone())).await;

        let check = MinimumValue::new(pgpool, 0).await;

        let deployment_id = test_models[0].deployment;
        let mut ctx = Context::new();
        ctx.insert(AgoraQuery {
            deployment_id,
            query: "query { a(skip: 10), b(bob: 5) }".into(),
            variables: "".into(),
        });

        let minimal_global_value = 20000000000000;
        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, minimal_global_value - 1)
                .await;
        let receipt = ReceiptWithState::new(signed_receipt);

        assert!(
            check.check(&ctx, &receipt).await.is_err(),
            "Should deny less than global"
        );

        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, minimal_global_value).await;
        let receipt = ReceiptWithState::new(signed_receipt);
        check
            .check(&ctx, &receipt)
            .await
            .expect("should accept equals global");

        let signed_receipt =
            create_signed_receipt(ALLOCATION_ID, u64::MAX, u64::MAX, minimal_global_value + 1)
                .await;
        let receipt = ReceiptWithState::new(signed_receipt);
        check
            .check(&ctx, &receipt)
            .await
            .expect("should accept more than global");
    }
}
