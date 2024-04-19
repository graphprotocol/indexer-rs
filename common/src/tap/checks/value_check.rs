// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::signers::Signature;
use anyhow::anyhow;
use bigdecimal::ToPrimitive;
use cost_model::CostModel;
use std::{
    cmp::min,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use thegraph_core::DeploymentId;
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use ttl_cache::TtlCache;

use tap_core::{
    receipt::{
        checks::{Check, CheckError, CheckResult},
        state::Checking,
        ReceiptWithState,
    },
    signed_message::{SignatureBytes, SignatureBytesExt},
};

pub struct MinimumValue {
    cost_model_cache: Arc<Mutex<HashMap<DeploymentId, CostModelCache>>>,
    query_ids: Arc<Mutex<HashMap<SignatureBytes, AgoraQuery>>>,
    model_handle: JoinHandle<()>,
    query_handle: JoinHandle<()>,
}

impl MinimumValue {
    pub fn new(
        mut rx_cost_model: Receiver<CostModelSource>,
        mut rx_query: Receiver<AgoraQuery>,
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
                            let _ = query.insert_model(value);
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
                            .insert(query.signature.get_signature_bytes(), query);
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
    fn get_agora_query(&self, query_id: &SignatureBytes) -> Option<AgoraQuery> {
        self.query_ids.lock().unwrap().remove(query_id)
    }

    fn get_expected_value(&self, query_id: &SignatureBytes) -> anyhow::Result<u128> {
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
        let key = &receipt.signed_receipt().signature.get_signature_bytes();

        let expected_value = self.get_expected_value(key).map_err(CheckError::Failed)?;

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
                "Query receipt does not have the minimum value. Expected value: {}. Minimum value: {}.",
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
    signature: Signature,
    deployment_id: DeploymentId,
    query: String,
    variables: String,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CostModelSource {
    deployment_id: DeploymentId,
    model: String,
    variables: String,
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
