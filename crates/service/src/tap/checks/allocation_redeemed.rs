// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use indexer_monitor::SubgraphClient;
use indexer_query::{payments_escrow_transactions_redeem, tap_transactions, TapTransactions};
use tap_core::receipt::checks::{Check, CheckError, CheckResult};
use thegraph_core::{
    alloy::{hex::ToHexExt, primitives::Address},
    CollectionId,
};

use crate::{
    middleware::Sender,
    tap::{CheckingReceipt, TapReceipt},
};

pub struct AllocationRedeemedCheck {
    indexer_address: Address,
    escrow_subgraph: Option<&'static SubgraphClient>,
    network_subgraph: Option<&'static SubgraphClient>,
}

impl AllocationRedeemedCheck {
    pub fn new(
        indexer_address: Address,
        escrow_subgraph: Option<&'static SubgraphClient>,
        network_subgraph: Option<&'static SubgraphClient>,
    ) -> Self {
        Self {
            indexer_address,
            escrow_subgraph,
            network_subgraph,
        }
    }

    async fn v1_allocation_redeemed(
        &self,
        sender: Address,
        allocation_id: Address,
    ) -> anyhow::Result<bool> {
        let escrow_subgraph = self
            .escrow_subgraph
            .ok_or_else(|| anyhow!("Missing escrow subgraph client for v1 receipts"))?;

        let response = escrow_subgraph
            .query::<TapTransactions, _>(tap_transactions::Variables {
                sender_id: sender.encode_hex(),
                receiver_id: self.indexer_address.encode_hex(),
                allocation_id: allocation_id.encode_hex(),
            })
            .await?;

        response
            .map(|data| !data.transactions.is_empty())
            .map_err(|err| anyhow!(err))
    }

    async fn v2_allocation_redeemed(
        &self,
        sender: Address,
        collection_id: CollectionId,
    ) -> anyhow::Result<bool> {
        let network_subgraph = self
            .network_subgraph
            .ok_or_else(|| anyhow!("Missing network subgraph client for v2 receipts"))?;

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
            .map_err(|err| anyhow!(err))
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for AllocationRedeemedCheck {
    async fn check(
        &self,
        ctx: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let Sender(sender) = ctx.get::<Sender>().ok_or_else(|| {
            CheckError::Failed(anyhow!("Could not find recovered sender in context"))
        })?;

        match receipt.signed_receipt() {
            TapReceipt::V1(r) => {
                let allocation_id = r.message.allocation_id;
                let redeemed = self
                    .v1_allocation_redeemed(*sender, allocation_id)
                    .await
                    .map_err(|e| CheckError::Failed(anyhow!(e)))?;
                if redeemed {
                    return Err(CheckError::Failed(anyhow!(
                        "Allocation already redeemed (v1): {}",
                        allocation_id
                    )));
                }
                Ok(())
            }
            TapReceipt::V2(r) => {
                let collection_id = CollectionId::from(r.message.collection_id);
                let redeemed = self
                    .v2_allocation_redeemed(*sender, collection_id)
                    .await
                    .map_err(|e| CheckError::Failed(anyhow!(e)))?;
                if redeemed {
                    return Err(CheckError::Failed(anyhow!(
                        "Allocation already redeemed (v2): {}",
                        collection_id.as_address()
                    )));
                }
                Ok(())
            }
        }
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
    use test_assets::{create_signed_receipt, TAP_SIGNER};
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
            AllocationRedeemedCheck::new(Address::from([0x22u8; 20]), None, Some(network_subgraph));

        let mut ctx = Context::default();
        ctx.insert(Sender(TAP_SIGNER.1));
        let receipt = create_v2_receipt(TAP_SIGNER.1);
        let checking = crate::tap::CheckingReceipt::new(receipt);

        let result = check.check(&ctx, &checking).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn v1_not_redeemed_allows() {
        let mock_server: MockServer = MockServer::start().await;
        mock_server
            .register(
                Mock::given(body_string_contains("TapTransactions")).respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": { "transactions": [] }
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

        let receipt =
            create_signed_receipt(test_assets::SignedReceiptRequest::builder().build()).await;
        let checking = crate::tap::CheckingReceipt::new(TapReceipt::V1(receipt));

        let check =
            AllocationRedeemedCheck::new(Address::from([0x22u8; 20]), Some(escrow_subgraph), None);

        let mut ctx = Context::default();
        ctx.insert(Sender(TAP_SIGNER.1));
        let result = check.check(&ctx, &checking).await;
        assert!(result.is_ok());
    }
}
