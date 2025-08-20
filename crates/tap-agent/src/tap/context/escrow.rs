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
