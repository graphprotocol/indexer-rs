// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use anyhow::Result;
use ethereum_types::U256;
use log::{error, info};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::graph_node::GraphNodeInstance;

#[derive(Debug)]
struct EscrowMonitorInner {
    graph_node: GraphNodeInstance,
    escrow_subgraph_deployment: String,
    indexer_address: Address,
    interval_ms: u64,
    sender_accounts: Arc<RwLock<HashMap<Address, U256>>>,
}

#[cfg_attr(test, faux::create)]
#[derive(Debug, Clone)]
pub struct EscrowMonitor {
    _monitor_handle: Arc<tokio::task::JoinHandle<()>>,
    inner: Arc<EscrowMonitorInner>,
}

#[cfg_attr(test, faux::methods)]
impl EscrowMonitor {
    pub async fn new(
        graph_node: GraphNodeInstance,
        escrow_subgraph_deployment: String,
        indexer_address: Address,
        interval_ms: u64,
    ) -> Result<Self> {
        let sender_accounts = Arc::new(RwLock::new(HashMap::new()));

        let inner = Arc::new(EscrowMonitorInner {
            graph_node,
            escrow_subgraph_deployment,
            indexer_address,
            interval_ms,
            sender_accounts,
        });

        let inner_clone = inner.clone();

        let monitor = EscrowMonitor {
            _monitor_handle: Arc::new(tokio::spawn(async move {
                EscrowMonitor::monitor_loop(&inner_clone).await.unwrap();
            })),
            inner,
        };

        Ok(monitor)
    }

    async fn current_accounts(
        graph_node: &GraphNodeInstance,
        escrow_subgraph_deployment: &str,
        indexer_address: &Address,
    ) -> Result<HashMap<Address, U256>> {
        // These 2 structs are used to deserialize the response from the escrow subgraph.
        // Note that U256's serde implementation is based on serializing the internal bytes, not the string decimal
        // representation. This is why we deserialize them as strings below.
        #[derive(Deserialize)]
        struct _Sender {
            id: Address,
        }
        #[derive(Deserialize)]
        struct _EscrowAccount {
            balance: String,
            #[serde(rename = "totalAmountThawing")]
            total_amount_thawing: String,
            sender: _Sender,
        }

        let res = graph_node
            .subgraph_query_raw(
                escrow_subgraph_deployment,
                serde_json::to_string(&json!({
                    "query": r#"
                        query ($indexer: ID!) {
                            escrowAccounts(where: {receiver_: {id: $indexer}}) {
                                balance
                                totalAmountThawing
                                sender {
                                    id
                                }
                            }
                        }
                    "#,
                    "variables": {
                        "indexer": indexer_address,
                    }
                  }
                ))
                .expect("serialize escrow GraphQL query"),
            )
            .await?;

        let mut res_json: serde_json::Value = serde_json::from_str(res.graphql_response.as_str())
            .map_err(|e| {
            anyhow::anyhow!(
                "Failed to fetch current accounts from escrow subgraph: {}",
                e
            )
        })?;

        let escrow_accounts: Vec<_EscrowAccount> =
            serde_json::from_value(res_json["data"]["escrowAccounts"].take()).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse current accounts response from escrow subgraph: {}",
                    e
                )
            })?;

        let mut sender_accounts: HashMap<Address, U256> = HashMap::new();

        for account in escrow_accounts {
            let balance = U256::checked_sub(
                U256::from_dec_str(&account.balance)?,
                U256::from_dec_str(&account.total_amount_thawing)?,
            )
            .unwrap_or_else(|| {
                error!(
                    "Balance minus total amount thawing underflowed for account {}. Setting balance to 0, no queries \
                    will be served for this sender.",
                    account.sender.id
                );
                U256::from(0)
            });

            sender_accounts.insert(account.sender.id, balance);
        }

        Ok(sender_accounts)
    }

    async fn update_accounts(inner: &Arc<EscrowMonitorInner>) -> Result<(), anyhow::Error> {
        *(inner.sender_accounts.write().await) = Self::current_accounts(
            &inner.graph_node,
            &inner.escrow_subgraph_deployment,
            &inner.indexer_address,
        )
        .await?;
        Ok(())
    }

    async fn monitor_loop(inner: &Arc<EscrowMonitorInner>) -> Result<()> {
        loop {
            match Self::update_accounts(inner).await {
                Ok(_) => {
                    info!("Updated escrow accounts");
                }
                Err(e) => {
                    error!("Error updating escrow accounts: {}", e);
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(inner.interval_ms)).await;
        }
    }

    pub async fn get_accounts(&self) -> tokio::sync::RwLockReadGuard<'_, HashMap<Address, U256>> {
        self.inner.sender_accounts.read().await
    }

    /// Returns true if the given address has a non-zero balance in the escrow contract.
    ///
    /// Note that this method does not take into account the outstanding TAP balance (Escrow balance - TAP receipts).
    pub async fn is_sender_eligible(&self, address: &Address) -> bool {
        self.inner.sender_accounts.read().await.get(address) > Some(&U256::from(0))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::{graph_node, test_vectors};

    use super::*;

    #[tokio::test]
    async fn test_current_accounts() {
        let indexer_address = Address::from_str(test_vectors::INDEXER_ADDRESS).unwrap();
        let escrow_subgraph_deployment = "Qmabcdefghijklmnopqrstuvwxyz1234567890ABCDEFGH";

        let mock_server = MockServer::start().await;
        let graph_node = graph_node::GraphNodeInstance::new(&mock_server.uri());

        let mock = Mock::given(method("POST"))
            .and(path(
                "/subgraphs/id/".to_string() + escrow_subgraph_deployment,
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_vectors::ESCROW_QUERY_RESPONSE, "application/json"),
            );
        mock_server.register(mock).await;

        let inner = EscrowMonitorInner {
            graph_node,
            escrow_subgraph_deployment: escrow_subgraph_deployment.to_string(),
            indexer_address,
            interval_ms: 1000,
            sender_accounts: Arc::new(RwLock::new(HashMap::new())),
        };

        let accounts = EscrowMonitor::current_accounts(
            &inner.graph_node,
            &inner.escrow_subgraph_deployment,
            &inner.indexer_address,
        )
        .await
        .unwrap();

        assert_eq!(accounts, test_vectors::expected_escrow_accounts());
    }
}
