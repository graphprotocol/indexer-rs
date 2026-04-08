// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use tap_core::manager::adapters::SignatureChecker;
use thegraph_core::alloy::primitives::Address;

use super::{error::AdapterError, NetworkVersion, TapAgentContext};

/// Implements the SignatureChecker for any [NetworkVersion]
#[async_trait]
impl<T: NetworkVersion + Send + Sync> SignatureChecker for TapAgentContext<T> {
    type AdapterError = AdapterError;

    async fn verify_signer(&self, signer: Address) -> Result<bool, Self::AdapterError> {
        let escrow_accounts = self.escrow_accounts.borrow();
        let sender = escrow_accounts
            .get_sender_for_signer(&signer)
            .map_err(|_| AdapterError::ValidationError {
                error: format!("Could not find the sender for the signer {signer}"),
            })?;

        let res = sender == self.sender;

        if !res {
            tracing::warn!(
                signer = %signer,
                expected_sender = %self.sender,
                "Signature verification failed",
            );
        }

        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use indexer_monitor::EscrowAccounts;
    use tap_core::manager::adapters::SignatureChecker;
    use thegraph_core::alloy::primitives::{Address, U256};
    use tokio::sync::watch;

    use super::super::{Horizon, TapAgentContext};

    const SENDER: Address = Address::new([0x01; 20]);
    const ACTIVE_SIGNER: Address = Address::new([0x02; 20]);
    const THAWING_SIGNER: Address = Address::new([0x03; 20]);

    /// Build a TapAgentContext whose escrow_accounts watcher contains
    /// only the given signer-to-sender mappings.
    async fn build_context(sender: Address, signers: Vec<Address>) -> TapAgentContext<Horizon> {
        let test_db = test_assets::setup_shared_test_db().await;
        let escrow = EscrowAccounts::new(
            HashMap::from([(sender, U256::from(1_000_000))]),
            HashMap::from([(sender, signers)]),
        );
        let (_, rx) = watch::channel(escrow);
        TapAgentContext::builder()
            .pgpool(test_db.pool)
            .sender(sender)
            .escrow_accounts(rx)
            .build()
    }

    #[tokio::test]
    async fn test_verify_signer_accepted_when_in_strict_set() {
        let ctx = build_context(SENDER, vec![ACTIVE_SIGNER]).await;
        let result = ctx.verify_signer(ACTIVE_SIGNER).await;
        assert!(result.unwrap(), "active signer should be accepted");
    }

    #[tokio::test]
    async fn test_verify_signer_rejected_when_excluded_from_strict_set() {
        // Strict watcher excludes THAWING_SIGNER — only ACTIVE_SIGNER is present
        let ctx = build_context(SENDER, vec![ACTIVE_SIGNER]).await;
        let result = ctx.verify_signer(THAWING_SIGNER).await;
        assert!(
            result.is_err(),
            "thawing signer not in strict set should be rejected"
        );
    }

    #[tokio::test]
    async fn test_verify_signer_rejected_when_sender_mismatch() {
        let other_sender = Address::new([0x99; 20]);
        // Signer maps to OTHER_SENDER, but context expects SENDER
        let escrow = EscrowAccounts::new(
            HashMap::from([
                (SENDER, U256::from(1_000_000)),
                (other_sender, U256::from(1_000_000)),
            ]),
            HashMap::from([(SENDER, vec![]), (other_sender, vec![ACTIVE_SIGNER])]),
        );
        let test_db = test_assets::setup_shared_test_db().await;
        let (_, rx) = watch::channel(escrow);
        let ctx = TapAgentContext::<Horizon>::builder()
            .pgpool(test_db.pool)
            .sender(SENDER)
            .escrow_accounts(rx)
            .build();
        let result = ctx.verify_signer(ACTIVE_SIGNER).await;
        assert!(
            !result.unwrap(),
            "signer mapped to wrong sender should return false"
        );
    }
}
