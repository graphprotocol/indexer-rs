// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::ToPrimitive;
use ethers::types::Signature;
use std::{
    cmp::min,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use thegraph::types::DeploymentId;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use ttl_cache::TtlCache;

use anyhow::anyhow;
use cost_model::CostModel;

use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
};

pub struct MinimumValue {
    cost_model_cache: Arc<Mutex<HashMap<DeploymentId, CostModelCache>>>,
    query_ids: Arc<Mutex<HashMap<Signature, AgoraQuery>>>,
    model_handle: JoinHandle<()>,
    query_handle: JoinHandle<()>,
}

#[derive(Clone)]
pub struct ValueCheckSender {
    pub tx_cost_model: Sender<CostModelSource>,
    pub tx_query: Sender<AgoraQuery>,
}

pub struct ValueCheckReceiver {
    rx_cost_model: Receiver<CostModelSource>,
    rx_query: Receiver<AgoraQuery>,
}

pub fn create_value_check(size: usize) -> (ValueCheckSender, ValueCheckReceiver) {
    let (tx_cost_model, rx_cost_model) = tokio::sync::mpsc::channel(size);
    let (tx_query, rx_query) = tokio::sync::mpsc::channel(size);

    (
        ValueCheckSender {
            tx_query,
            tx_cost_model,
        },
        ValueCheckReceiver {
            rx_cost_model,
            rx_query,
        },
    )
}

impl MinimumValue {
    pub fn new(
        ValueCheckReceiver {
            mut rx_query,
            mut rx_cost_model,
        }: ValueCheckReceiver,
    ) -> Self {
        let cost_model_cache = Arc::new(Mutex::new(HashMap::<DeploymentId, CostModelCache>::new()));
        let query_ids = Arc::new(Mutex::new(HashMap::new()));
        let cache = cost_model_cache.clone();
        let query_ids_clone = query_ids.clone();
        let model_handle = tokio::spawn(async move {
            loop {
                let model = rx_cost_model.recv().await;
                match model {
                    Some(value) => {
                        let deployment_id = value.deployment_id;

                        if let Some(query) = cache.lock().unwrap().get_mut(&deployment_id) {
                            let _ = query.insert_model(value).inspect_err(|err| {
                                tracing::error!(
                                    "Error while compiling cost model for deployment id {}. Error: {}",
                                    deployment_id, err
                                )
                            });
                        } else {
                            match CostModelCache::new(value) {
                                Ok(value) => {
                                    cache.lock().unwrap().insert(deployment_id, value);
                                }
                                Err(err) => {
                                    tracing::error!(
                                        "Error while compiling cost model for deployment id {}. Error: {}",
                                        deployment_id, err
                                    )
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
        });

        // we use two different handles because in case one channel breaks we still have the other
        let query_handle = tokio::spawn(async move {
            loop {
                let query = rx_query.recv().await;
                match query {
                    Some(query) => {
                        query_ids_clone
                            .lock()
                            .unwrap()
                            .insert(query.signature, query);
                    }
                    None => break,
                }
            }
        });

        Self {
            cost_model_cache,
            model_handle,
            query_ids,
            query_handle,
        }
    }
}

impl Drop for MinimumValue {
    fn drop(&mut self) {
        self.model_handle.abort();
        self.query_handle.abort();
    }
}

impl MinimumValue {
    fn get_agora_query(&self, query_id: &Signature) -> Option<AgoraQuery> {
        self.query_ids.lock().unwrap().remove(query_id)
    }

    fn get_expected_value(&self, query_id: &Signature) -> anyhow::Result<u128> {
        // get query from key
        let agora_query = self
            .get_agora_query(query_id)
            .ok_or(anyhow!("No query found"))?;

        // get agora model for the allocation_id
        let mut cache = self.cost_model_cache.lock().unwrap();
        // on average, we'll have zero or one model
        let models = cache.get_mut(&agora_query.deployment_id);

        let expected_value = models
            .map(|cache| cache.cost(&agora_query))
            .unwrap_or_default();

        Ok(expected_value)
    }
}

#[async_trait::async_trait]
impl Check for MinimumValue {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        // get key
        let key = &receipt.signed_receipt().signature;

        let expected_value = self.get_expected_value(key)?;

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
            return Err(anyhow!(
                "Query receipt does not have the minimum value. Expected value: {}. Minimum value: {}.",
                expected_value, value,
            ));
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
    pub signature: Signature,
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
