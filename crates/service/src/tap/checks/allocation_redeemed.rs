// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_monitor::{SubgraphClient, SubgraphQueryError};
use indexer_query::closed_allocations::{self, ClosedAllocations};
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address},
    CollectionId,
};

use crate::tap::{CheckingReceipt, TapReceipt};

/// Errors that can occur during allocation redemption checks.
#[derive(Debug, thiserror::Error)]
pub enum AllocationCheckError {
    #[error("Missing network subgraph client for v2 receipts")]
    MissingNetworkSubgraph,

    #[error("Subgraph query failed")]
    SubgraphQuery(#[from] SubgraphQueryError),
}

pub struct AllocationRedeemedCheck {
    indexer_address: Address,
    network_subgraph: Option<&'static SubgraphClient>,
}

impl AllocationRedeemedCheck {
    pub fn new(
        indexer_address: Address,
        network_subgraph: Option<&'static SubgraphClient>,
    ) -> Self {
        Self {
            indexer_address,
            network_subgraph,
        }
    }

    async fn v2_allocation_closed(
        &self,
        collection_id: CollectionId,
    ) -> Result<bool, AllocationCheckError> {
        let network_subgraph = self
            .network_subgraph
            .ok_or(AllocationCheckError::MissingNetworkSubgraph)?;

        // Horizon network subgraph stores allocationId as the 20-byte address derived
        // from the 32-byte collection_id (rightmost 20 bytes).
        let allocation_id = collection_id.as_address().encode_hex();

        // Only reject receipts if the allocation is actually closed on-chain.
        // In the continuous collection model, active allocations get collected
        // from periodically — redeem transactions existing doesn't mean the
        // allocation is done.
        let closed_response = network_subgraph
            .query::<ClosedAllocations, _>(closed_allocations::Variables {
                allocation_ids: vec![allocation_id],
                block: None,
                first: 1,
                last: String::new(),
            })
            .await?;

        Ok(!closed_response.allocations.is_empty())
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationRedeemedCheck {
    async fn check(
        &self,
        _ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let collection_id =
            CollectionId::from(receipt.signed_receipt().as_ref().message.collection_id);
        let closed = self
            .v2_allocation_closed(collection_id)
            .await
            .map_err(|e| CheckError::Failed(anyhow::anyhow!(e)))?;
        if closed {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Allocation is closed (v2): {}",
                collection_id.as_address()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use indexer_monitor::{DeploymentDetails, SubgraphClient};
    use serde_json::json;
    use tap_core::{
        receipt::{checks::Check, Context},
        signed_message::Eip712SignedMessage,
        tap_eip712_domain, TapVersion,
    };
    use tap_graph::v2::Receipt as ReceiptV2;
    use test_assets::TAP_SIGNER;
    use thegraph_core::alloy::{
        primitives::{Address, FixedBytes},
        signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
    };
    use wiremock::{matchers::body_string_contains, Mock, MockServer, ResponseTemplate};

    use super::AllocationRedeemedCheck;
    use crate::tap::TapReceipt;

    fn create_wallet() -> PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
            .index(0)
            .unwrap()
            .build()
            .unwrap()
    }

    fn create_v2_receipt(payer: Address) -> TapReceipt {
        let wallet = create_wallet();
        let eip712_domain = tap_eip712_domain(1, Address::from([0x11u8; 20]), TapVersion::V2);
        let receipt = ReceiptV2 {
            payer,
            data_service: Address::ZERO,
            service_provider: Address::ZERO,
            collection_id: FixedBytes::ZERO,
            timestamp_ns: 1000,
            nonce: 1,
            value: 100,
        };
        let signed = Eip712SignedMessage::new(&eip712_domain, receipt, &wallet).unwrap();
        TapReceipt(signed)
    }

    #[tokio::test]
    async fn v2_closed_allocation_rejects() {
        let mock_server: MockServer = MockServer::start().await;
        mock_server
            .register(
                Mock::given(body_string_contains("allocations")).respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "meta": { "block": { "number": 1, "hash": "0x00", "timestamp": 1 } },
                            "allocations": [ { "id": "0x01" } ]
                        }
                    })),
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

        let check =
            AllocationRedeemedCheck::new(Address::from([0x22u8; 20]), Some(network_subgraph));

        let ctx = Context::default();
        let receipt = create_v2_receipt(TAP_SIGNER.1);
        let checking = crate::tap::CheckingReceipt::new(receipt);

        let result = check.check(&ctx, &checking).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn v2_open_allocation_passes() {
        let mock_server: MockServer = MockServer::start().await;
        mock_server
            .register(
                Mock::given(body_string_contains("allocations")).respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "meta": { "block": { "number": 1, "hash": "0x00", "timestamp": 1 } },
                            "allocations": []
                        }
                    })),
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

        let check =
            AllocationRedeemedCheck::new(Address::from([0x22u8; 20]), Some(network_subgraph));

        let ctx = Context::default();
        let receipt = create_v2_receipt(TAP_SIGNER.1);
        let checking = crate::tap::CheckingReceipt::new(receipt);

        let result = check.check(&ctx, &checking).await;
        assert!(result.is_ok());
    }
}
