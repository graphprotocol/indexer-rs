// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use indexer_monitor::{SubgraphClient, SubgraphQueryError};
use indexer_query::payments_escrow_transactions_redeem;
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address},
    CollectionId,
};

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

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

    async fn v2_allocation_redeemed(
        &self,
        sender: Address,
        collection_id: CollectionId,
    ) -> Result<bool, AllocationCheckError> {
        let network_subgraph = self
            .network_subgraph
            .ok_or(AllocationCheckError::MissingNetworkSubgraph)?;

        // Horizon network subgraph stores allocationId as the 20-byte address derived
        // from the 32-byte collection_id (rightmost 20 bytes).
        let allocation_ids = vec![collection_id.as_address().encode_hex()];

        let response = network_subgraph
            .query::<payments_escrow_transactions_redeem::PaymentsEscrowTransactionsRedeemQuery, _>(
                payments_escrow_transactions_redeem::Variables {
                    payer: sender.encode_hex(),
                    receiver: self.indexer_address.encode_hex(),
                    allocation_ids: Some(allocation_ids),
                },
            )
            .await?;

        response
            .map(|data| !data.payments_escrow_transactions.is_empty())
            .map_err(AllocationCheckError::SubgraphQuery)
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationRedeemedCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(sender) = ctx
            .get::<Sender>()
            .ok_or_else(|| CheckError::Failed(anyhow::anyhow!("Missing sender in context")))?;

        let collection_id = CollectionId::from(
            receipt
                .signed_receipt()
                .get_v2_receipt()
                .message
                .collection_id,
        );
        let redeemed = self
            .v2_allocation_redeemed(*sender, collection_id)
            .await
            .map_err(|e| CheckError::Failed(anyhow::anyhow!(e)))?;
        if redeemed {
            return Err(CheckError::Failed(anyhow::anyhow!(
                "Allocation already redeemed (v2): {}",
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
    use crate::{middleware::Sender, tap::TapReceipt};

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
        TapReceipt::V2(signed)
    }

    #[tokio::test]
    async fn v2_redeemed_rejects() {
        let mock_server: MockServer = MockServer::start().await;
        mock_server
            .register(
                Mock::given(body_string_contains("paymentsEscrowTransactions")).respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": { "paymentsEscrowTransactions": [ { "id": "0x01", "allocationId": TAP_SIGNER.1.to_string(), "timestamp": "1" } ] }
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

        let mut ctx = Context::default();
        ctx.insert(Sender(TAP_SIGNER.1));
        let receipt = create_v2_receipt(TAP_SIGNER.1);
        let checking = crate::tap::CheckingReceipt::new(receipt);

        let result = check.check(&ctx, &checking).await;
        assert!(result.is_err());
    }
}
