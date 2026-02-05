// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use anyhow::anyhow;
use indexer_monitor::SubgraphClient;
use indexer_query::{payments_escrow_transactions_redeem, tap_transactions, TapTransactions};
use indexer_watcher::new_watcher;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address},
    CollectionId,
};
use tokio::sync::watch::Receiver;

use crate::tap::{CheckingReceipt, TapReceipt};

/// AllocationId check
///
/// Verifies if the allocation is already redeemed.
pub struct AllocationId {
    tap_allocation_redeemed: Receiver<bool>,
    allocation_id: Address,
    collection_id: Option<CollectionId>,
}

impl AllocationId {
    /// Creates a new allocation id check
    pub async fn new(
        indexer_address: Address,
        escrow_polling_interval: Duration,
        sender_id: Address,
        allocation_id: Address,
        collection_id: Option<CollectionId>,
        escrow_subgraph: &'static SubgraphClient,
        network_subgraph: &'static SubgraphClient,
    ) -> Self {
        let tap_allocation_redeemed = tap_allocation_redeemed_watcher(
            allocation_id,
            collection_id,
            sender_id,
            indexer_address,
            escrow_subgraph,
            network_subgraph,
            escrow_polling_interval,
        )
        .await
        .expect("Failed to initialize tap_allocation_redeemed_watcher");

        Self {
            tap_allocation_redeemed,
            allocation_id,
            collection_id,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationId {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        // Support both Legacy (V1) and Horizon (V2) receipts.
        // V1 provides allocation_id directly; V2 provides collection_id which we map to an Address.
        let allocation_id = if let Some(a) = receipt.signed_receipt().allocation_id() {
            a
        } else if let Some(cid) = receipt.signed_receipt().collection_id() {
            // V2: collection_id is 32 bytes with the 20-byte address right-aligned (left-padded zeros).
            let bytes = cid.as_slice();
            if bytes.len() != 32 {
                return Err(CheckError::Failed(anyhow!(
                    "Invalid collection_id length: {} (expected 32)",
                    bytes.len()
                )));
            }
            Address::from_slice(&bytes[12..32])
        } else {
            return Err(CheckError::Failed(anyhow!(
                "Receipt does not have an allocation_id or collection_id"
            )));
        };

        tracing::debug!(
            allocation_id = %allocation_id,
            expected_allocation_id = %self.allocation_id,
            "Checking allocation_id",
        );
        if allocation_id != self.allocation_id {
            return Err(CheckError::Failed(anyhow!("Receipt allocation_id different from expected: allocation_id: {:?}, expected_allocation_id: {}", allocation_id, self.allocation_id)));
        };

        // Check that the allocation ID is not redeemed yet for this consumer
        match *self.tap_allocation_redeemed.borrow() {
            false => Ok(()),
            true => Err(CheckError::Failed(anyhow!(
                "Allocation {:?} already redeemed",
                self.collection_id
                    .map(|collection_id| collection_id.encode_hex())
                    .unwrap_or_else(|| allocation_id.to_string())
            ))),
        }
    }
}

async fn tap_allocation_redeemed_watcher(
    allocation_id: Address,
    collection_id: Option<CollectionId>,
    sender_address: Address,
    indexer_address: Address,
    escrow_subgraph: &'static SubgraphClient,
    network_subgraph: &'static SubgraphClient,
    escrow_polling_interval: Duration,
) -> anyhow::Result<Receiver<bool>> {
    new_watcher(escrow_polling_interval, move || async move {
        match collection_id {
            Some(collection_id) => {
                query_network_redeem_transactions(
                    collection_id,
                    sender_address,
                    indexer_address,
                    network_subgraph,
                )
                .await
            }
            None => {
                query_escrow_check_transactions(
                    allocation_id,
                    sender_address,
                    indexer_address,
                    escrow_subgraph,
                )
                .await
            }
        }
    })
    .await
}

async fn query_escrow_check_transactions(
    allocation_id: Address,
    sender_address: Address,
    indexer_address: Address,
    escrow_subgraph: &'static SubgraphClient,
) -> anyhow::Result<bool> {
    let data = escrow_subgraph
        .query::<TapTransactions, _>(tap_transactions::Variables {
            sender_id: sender_address.to_string().to_lowercase(),
            receiver_id: indexer_address.to_string().to_lowercase(),
            allocation_id: allocation_id.to_string().to_lowercase(),
        })
        .await?;

    Ok(!data.transactions.is_empty())
}

async fn query_network_redeem_transactions(
    collection_id: CollectionId,
    sender_address: Address,
    indexer_address: Address,
    network_subgraph: &'static SubgraphClient,
) -> anyhow::Result<bool> {
    // Horizon network subgraph stores allocationId as the 20-byte address derived
    // from the 32-byte collection_id (rightmost 20 bytes).
    let allocation_ids = vec![collection_id.as_address().encode_hex()];
    let data = network_subgraph
        .query::<payments_escrow_transactions_redeem::PaymentsEscrowTransactionsRedeemQuery, _>(
            payments_escrow_transactions_redeem::Variables {
                payer: sender_address.encode_hex(),
                receiver: indexer_address.encode_hex(),
                allocation_ids: Some(allocation_ids),
            },
        )
        .await?;

    Ok(!data.payments_escrow_transactions.is_empty())
}

#[cfg(test)]
mod tests {
    use indexer_monitor::{DeploymentDetails, SubgraphClient};
    use serde_json::json;
    use thegraph_core::{alloy::hex::ToHexExt, CollectionId};
    use wiremock::{matchers::body_string_contains, Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_transaction_exists() {
        let allocation_id = "0x43f8ebe0b6181117eb2dcf8ec7d4e894fca060b8";
        let sender_address = "0x21fed3c4340f67dbf2b78c670ebd1940668ca03e";
        let indexer_address = "0x54d7db28ce0d0e2e87764cd09298f9e4e913e567";

        let mock_server: MockServer = MockServer::start().await;
        mock_server
            .register(
                Mock::given(body_string_contains("TapTransactions")).respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "data": {
                            "transactions": [
                                { "id": "0x01" }
                            ]
                        }
                    })),
                ),
            )
            .await;

        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
            )
            .await,
        ));

        let result = super::query_escrow_check_transactions(
            allocation_id.parse().unwrap(),
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            escrow_subgraph,
        );

        assert!(result.await.unwrap());
    }

    #[tokio::test]
    async fn test_network_redeem_transactions_true_when_present() {
        let mock_server: MockServer = MockServer::start().await;
        let sender_address = "0x21fed3c4340f67dbf2b78c670ebd1940668ca03e";
        let indexer_address = "0x54d7db28ce0d0e2e87764cd09298f9e4e913e567";
        let collection_id = CollectionId::from(
            sender_address
                .parse::<thegraph_core::alloy::primitives::Address>()
                .unwrap(),
        );

        mock_server
            .register(
                Mock::given(body_string_contains("paymentsEscrowTransactions"))
                    .and(body_string_contains(collection_id.as_address().encode_hex()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "paymentsEscrowTransactions": [
                                { "id": "0x01", "allocationId": collection_id.as_address().encode_hex(), "timestamp": "1" }
                            ]
                        }
                    }))),
            )
            .await;

        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
            )
            .await,
        ));

        let result = super::query_network_redeem_transactions(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await
        .unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_network_redeem_transactions_false_when_empty() {
        let mock_server: MockServer = MockServer::start().await;
        let sender_address = "0x21fed3c4340f67dbf2b78c670ebd1940668ca03e";
        let indexer_address = "0x54d7db28ce0d0e2e87764cd09298f9e4e913e567";
        let collection_id = CollectionId::from(
            sender_address
                .parse::<thegraph_core::alloy::primitives::Address>()
                .unwrap(),
        );

        mock_server
            .register(
                Mock::given(body_string_contains("paymentsEscrowTransactions")).respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "data": { "paymentsEscrowTransactions": [] } })),
                ),
            )
            .await;

        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
            )
            .await,
        ));

        let result = super::query_network_redeem_transactions(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await
        .unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_network_redeem_transactions_error_when_subgraph_fails() {
        let mock_server: MockServer = MockServer::start().await;
        let sender_address = "0x21fed3c4340f67dbf2b78c670ebd1940668ca03e";
        let indexer_address = "0x54d7db28ce0d0e2e87764cd09298f9e4e913e567";
        let collection_id = CollectionId::from(
            sender_address
                .parse::<thegraph_core::alloy::primitives::Address>()
                .unwrap(),
        );

        mock_server
            .register(
                Mock::given(body_string_contains("paymentsEscrowTransactions")).respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "errors": [{ "message": "boom" }] })),
                ),
            )
            .await;

        let network_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&mock_server.uri()).unwrap(),
            )
            .await,
        ));

        let result = super::query_network_redeem_transactions(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await;

        assert!(result.is_err());
    }
}
