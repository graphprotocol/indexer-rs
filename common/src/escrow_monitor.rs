// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::Address;
use anyhow::Result;
use ethers_core::types::U256;
use log::{error, info, warn};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::prelude::SubgraphClient;

#[derive(Debug)]
struct EscrowMonitorInner {
    escrow_subgraph: &'static SubgraphClient,
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
        escrow_subgraph: &'static SubgraphClient,
        indexer_address: Address,
        interval_ms: u64,
    ) -> Result<Self> {
        let sender_accounts = Arc::new(RwLock::new(HashMap::new()));

        let inner = Arc::new(EscrowMonitorInner {
            escrow_subgraph,
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
        escrow_subgraph: &'static SubgraphClient,
        indexer_address: &Address,
    ) -> Result<HashMap<Address, U256>> {
        // Types for deserializing the network subgraph response
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EscrowAccountsResponse {
            escrow_accounts: Vec<EscrowAccount>,
        }
        // These 2 structs are used to deserialize the response from the escrow subgraph.
        // Note that U256's serde implementation is based on serializing the internal bytes, not the string decimal
        // representation. This is why we deserialize them as strings below.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EscrowAccount {
            balance: String,
            total_amount_thawing: String,
            sender: Sender,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Sender {
            id: Address,
        }

        let response = escrow_subgraph
            .query::<EscrowAccountsResponse>(&json!({
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
            .await?;

        // If there are any GraphQL errors returned, we'll log them for debugging
        if let Some(errors) = response.errors {
            warn!(
                "Errors encountered fetching escrow accounts for indexer {:?}: {}",
                indexer_address,
                errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let sender_accounts = response
            .data
            .map_or(vec![], |data| data.escrow_accounts)
            .iter()
            .map(|account| {
                let balance = U256::checked_sub(
                    U256::from_str_radix(&account.balance, 16)?,
                    U256::from_str_radix(&account.total_amount_thawing, 16)?,
                )
                .unwrap_or_else(|| {
                    warn!(
                        "Balance minus total amount thawing underflowed for account {}. \
                         Setting balance to 0, no queries will be served for this sender.",
                        account.sender.id
                    );
                    U256::from(0)
                });

                Ok((account.sender.id, balance))
            })
            .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

        Ok(sender_accounts)
    }

    async fn update_accounts(inner: &Arc<EscrowMonitorInner>) -> Result<(), anyhow::Error> {
        *(inner.sender_accounts.write().await) =
            Self::current_accounts(inner.escrow_subgraph, &inner.indexer_address).await?;
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::test_vectors;
    use crate::test_vectors::{ESCROW_SUBGRAPH_DEPLOYMENT, INDEXER_ADDRESS};

    use super::*;

    #[tokio::test]
    async fn test_current_accounts() {
        // Set up a mock escrow subgraph
        let mock_server = MockServer::start().await;
        let escrow_subgraph_endpoint = SubgraphClient::local_deployment_endpoint(
            &mock_server.uri(),
            &test_vectors::ESCROW_SUBGRAPH_DEPLOYMENT,
        )
        .unwrap();
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                Some(&mock_server.uri()),
                Some(&test_vectors::ESCROW_SUBGRAPH_DEPLOYMENT),
                escrow_subgraph_endpoint.as_ref(),
            )
            .unwrap(),
        ));

        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                *ESCROW_SUBGRAPH_DEPLOYMENT
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_vectors::ESCROW_QUERY_RESPONSE, "application/json"),
            );
        mock_server.register(mock).await;

        let accounts = EscrowMonitor::current_accounts(escrow_subgraph, &INDEXER_ADDRESS)
            .await
            .unwrap();

        assert_eq!(accounts, *test_vectors::ESCROW_ACCOUNTS);
    }
}
