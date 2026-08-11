// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use anyhow::anyhow;
use indexer_monitor::SubgraphClient;
use indexer_query::payments_escrow_transactions_redeem;
use indexer_watcher::new_watcher;
use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    WithValueAndTimestamp,
};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address},
    CollectionId,
};
use tokio::sync::watch::Receiver;

use crate::tap::{CheckingReceipt, TapReceipt};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// AllocationId check
///
/// Verifies that a receipt is newer than the allocation's most recent on-chain redemption.
/// Redemption no longer implies the allocation is closed — indexer-agent redeems RAVs on open
/// allocations on a timer — so only receipts at or older than the last redemption are replays.
pub struct AllocationId {
    last_redeemed_at_secs: Receiver<Option<u64>>,
    allocation_id: Address,
    collection_id: CollectionId,
}

impl AllocationId {
    /// Creates a new allocation id check
    pub async fn new(
        indexer_address: Address,
        escrow_polling_interval: Duration,
        sender_id: Address,
        allocation_id: Address,
        collection_id: CollectionId,
        network_subgraph: &'static SubgraphClient,
    ) -> Self {
        let last_redeemed_at_secs = tap_allocation_redeemed_watcher(
            collection_id,
            sender_id,
            indexer_address,
            network_subgraph,
            escrow_polling_interval,
        )
        .await
        .expect("Failed to initialize tap_allocation_redeemed_watcher");

        Self {
            last_redeemed_at_secs,
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
        let collection_id = receipt.signed_receipt().collection_id();
        if self.collection_id != collection_id {
            return Err(CheckError::Failed(anyhow!(
                "Receipt collection_id different from expected: collection_id: {:?}, expected_collection_id: {}",
                collection_id,
                self.collection_id
            )));
        }
        let allocation_id = self.collection_id.as_address();

        tracing::debug!(
            allocation_id = %allocation_id,
            expected_allocation_id = %self.allocation_id,
            "Checking allocation_id",
        );
        if allocation_id != self.allocation_id {
            return Err(CheckError::Failed(anyhow!("Receipt allocation_id different from expected: allocation_id: {:?}, expected_allocation_id: {}", allocation_id, self.allocation_id)));
        };

        let Some(last_redeemed_at_secs) = *self.last_redeemed_at_secs.borrow() else {
            return Ok(());
        };
        let last_redeemed_at_ns = last_redeemed_at_secs
            .checked_mul(NANOS_PER_SECOND)
            .ok_or_else(|| {
                CheckError::Failed(anyhow!(
                    "Last redeemed timestamp {last_redeemed_at_secs}s overflows when converted to nanoseconds"
                ))
            })?;
        let receipt_timestamp_ns = receipt.signed_receipt().timestamp_ns();

        if receipt_timestamp_ns <= last_redeemed_at_ns {
            return Err(CheckError::Failed(anyhow!(
                "Receipt timestamp {receipt_timestamp_ns}ns for allocation {:?} is not newer than the last redemption at {last_redeemed_at_secs}s ({last_redeemed_at_ns}ns)",
                self.collection_id.encode_hex()
            )));
        }

        Ok(())
    }
}

async fn tap_allocation_redeemed_watcher(
    collection_id: CollectionId,
    sender_address: Address,
    indexer_address: Address,
    network_subgraph: &'static SubgraphClient,
    escrow_polling_interval: Duration,
) -> anyhow::Result<Receiver<Option<u64>>> {
    new_watcher(escrow_polling_interval, move || async move {
        query_latest_redeem_timestamp_secs(
            collection_id,
            sender_address,
            indexer_address,
            network_subgraph,
        )
        .await
    })
    .await
}

/// Returns `None` if the allocation has never been redeemed.
async fn query_latest_redeem_timestamp_secs(
    collection_id: CollectionId,
    sender_address: Address,
    indexer_address: Address,
    network_subgraph: &'static SubgraphClient,
) -> anyhow::Result<Option<u64>> {
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

    let mut latest_redeemed_at_secs: Option<u64> = None;
    for transaction in &data.payments_escrow_transactions {
        let timestamp: u64 = transaction.timestamp.parse().map_err(|e| {
            anyhow!(
                "Invalid redeem transaction timestamp {:?}: {e}",
                transaction.timestamp
            )
        })?;
        latest_redeemed_at_secs = latest_redeemed_at_secs.max(Some(timestamp));
    }

    Ok(latest_redeemed_at_secs)
}

#[cfg(test)]
mod tests {
    use indexer_monitor::{DeploymentDetails, SubgraphClient};
    use serde_json::json;
    use tap_core::receipt::{checks::Check, Context};
    use test_assets::{ALLOCATION_ID_0, COLLECTION_ID_0, TAP_SIGNER as SIGNER};
    use thegraph_core::{alloy::hex::ToHexExt, CollectionId};
    use tokio::sync::watch;
    use wiremock::{matchers::body_string_contains, Mock, MockServer, ResponseTemplate};

    use crate::test::create_received_receipt_v2;

    /// Builds the check directly, bypassing `new`'s network watcher, with a fixed redemption time.
    fn allocation_id_check(last_redeemed_at_secs: Option<u64>) -> super::AllocationId {
        let (_tx, rx) = watch::channel(last_redeemed_at_secs);
        super::AllocationId {
            last_redeemed_at_secs: rx,
            allocation_id: ALLOCATION_ID_0,
            collection_id: COLLECTION_ID_0,
        }
    }

    #[tokio::test]
    async fn test_latest_redeem_timestamp_returns_latest_timestamp() {
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
                                { "id": "0x01", "allocationId": collection_id.as_address().encode_hex(), "timestamp": "5" },
                                { "id": "0x02", "allocationId": collection_id.as_address().encode_hex(), "timestamp": "9" },
                                { "id": "0x03", "allocationId": collection_id.as_address().encode_hex(), "timestamp": "3" }
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

        let result = super::query_latest_redeem_timestamp_secs(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await
        .unwrap();

        // The largest timestamp is deliberately not the last row: an allocation redeemed
        // more than once must yield the most recent redemption, whatever order the
        // subgraph returns the transactions in.
        assert_eq!(result, Some(9));
    }

    #[tokio::test]
    async fn test_latest_redeem_timestamp_returns_none_when_empty() {
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

        let result = super::query_latest_redeem_timestamp_secs(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await
        .unwrap();

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_latest_redeem_timestamp_error_when_subgraph_fails() {
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

        let result = super::query_latest_redeem_timestamp_secs(
            collection_id,
            sender_address.parse().unwrap(),
            indexer_address.parse().unwrap(),
            network_subgraph,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_receipt_newer_than_redemption_is_accepted() {
        let redeemed_at_secs = 1_700_000_000u64;
        let check = allocation_id_check(Some(redeemed_at_secs));

        let receipt_timestamp_ns = (redeemed_at_secs + 10) * 1_000_000_000;
        let receipt =
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, receipt_timestamp_ns, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_ok(),
            "expected a receipt newer than the last redemption to be accepted, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_receipt_at_or_older_than_redemption_is_rejected() {
        let redeemed_at_secs = 1_700_000_000u64;
        let check = allocation_id_check(Some(redeemed_at_secs));

        let receipt_timestamp_ns = redeemed_at_secs * 1_000_000_000;
        let receipt =
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, receipt_timestamp_ns, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_err(),
            "expected a receipt at the last redemption to still be rejected (anti-replay)"
        );

        let receipt_timestamp_ns = (redeemed_at_secs - 10) * 1_000_000_000;
        let receipt =
            create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 2, receipt_timestamp_ns, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_err(),
            "expected a receipt older than the last redemption to be rejected (anti-replay)"
        );
    }

    #[tokio::test]
    async fn test_no_redemption_accepts_receipt() {
        let check = allocation_id_check(None);
        let receipt = create_received_receipt_v2(&ALLOCATION_ID_0, &SIGNER.0, 1, 1, 1);

        let result = check.check(&Context::new(), &receipt).await;

        assert!(
            result.is_ok(),
            "expected a receipt to be accepted when the allocation was never redeemed, got {:?}",
            result
        );
    }
}
