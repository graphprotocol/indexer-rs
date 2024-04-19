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
};
use thegraph_core::DeploymentId;
use tokio::{select, sync::mpsc::Receiver, task::JoinHandle};

use tap_core::{
    receipt::{
        checks::{Check, CheckError, CheckResult},
        state::Checking,
        ReceiptWithState,
    },
    signed_message::{SignatureBytes, SignatureBytesExt},
};

pub struct MinimumValue {
    cost_model_cache: Arc<Mutex<HashMap<DeploymentId, CostModel>>>,
    query_ids: Arc<Mutex<HashMap<SignatureBytes, AgoraQuery>>>,
    handle: JoinHandle<()>,
}

impl MinimumValue {
    pub fn new(
        mut rx_cost_model: Receiver<CostModelSource>,
        mut rx_query: Receiver<AgoraQuery>,
    ) -> Self {
        let cost_model_cache = Arc::new(Mutex::new(HashMap::new()));
        let query_ids = Arc::new(Mutex::new(HashMap::new()));
        let cache = cost_model_cache.clone();
        let query_ids_clone = query_ids.clone();
        let handle = tokio::spawn(async move {
            loop {
                select! {
                    model = rx_cost_model.recv() => {
                        match model {
                            Some(value) => {
                                let deployment_id = value.deployment_id;

                                match compile_cost_model(value) {
                                    Ok(value) => {
                                        // todo keep track of the last X models
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
                            None => continue,
                        }
                    }
                    query = rx_query.recv() => {
                        match query {
                            Some(query) => {
                                query_ids_clone.lock().unwrap().insert(query.signature.get_signature_bytes(), query);
                            },
                            None => continue,
                        }
                    }
                }
            }
        });

        Self {
            cost_model_cache,
            handle,
            query_ids,
        }
    }
}

impl Drop for MinimumValue {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[async_trait::async_trait]
impl Check for MinimumValue {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        // get key
        let key = &receipt.signed_receipt().signature.get_signature_bytes();

        // get query from key
        let agora_query = self
            .query_ids
            .lock()
            .unwrap()
            .remove(key)
            .ok_or(anyhow!("No query found"))
            .map_err(CheckError::Failed)?;

        // get agora model for the allocation_id
        let cache = self.cost_model_cache.lock().unwrap();

        // on average, we'll have zero or one model
        let models = cache
            .get(&agora_query.deployment_id)
            .map(|model| vec![model])
            .unwrap_or_default();

        // get value
        let value = receipt.signed_receipt().message.value;

        let expected_value = models
            .into_iter()
            .fold(None, |acc, model| {
                let value = model
                    .cost(&agora_query.query, &agora_query.variables)
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
            .unwrap_or_default();

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

fn compile_cost_model(src: CostModelSource) -> Result<CostModel, String> {
    if src.model.len() > (1 << 16) {
        return Err("CostModelTooLarge".into());
    }
    let model = CostModel::compile(&src.model, &src.variables).map_err(|err| err.to_string())?;
    Ok(model)
}

pub struct AgoraQuery {
    signature: Signature,
    deployment_id: DeploymentId,
    query: String,
    variables: String,
}

#[derive(Eq, Hash, PartialEq)]
pub struct CostModelSource {
    deployment_id: DeploymentId,
    model: String,
    variables: String,
}
