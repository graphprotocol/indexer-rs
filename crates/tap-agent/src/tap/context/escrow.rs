// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use tap_core::manager::adapters::SignatureChecker;
use thegraph_core::alloy::primitives::Address;

use super::{error::AdapterError, NetworkVersion, TapAgentContext};

// Conversion from eventuals::error::Closed to AdapterError::EscrowEventualError
impl From<eventuals::error::Closed> for AdapterError {
    fn from(e: eventuals::error::Closed) -> Self {
        AdapterError::EscrowEventualError {
            error: format!("{:?}", e),
        }
    }
}

/// Implements the SignatureChecker for any [NetworkVersion]
#[async_trait]
impl<T: NetworkVersion + Send + Sync> SignatureChecker for TapAgentContext<T> {
    type AdapterError = AdapterError;

    async fn verify_signer(&self, signer: Address) -> Result<bool, Self::AdapterError> {
        let escrow_accounts = self.escrow_accounts.borrow();
        let sender = escrow_accounts
            .get_sender_for_signer(&signer)
            .map_err(|_| AdapterError::ValidationError {
                error: format!("Could not find the sender for the signer {}", signer),
            })?;
        Ok(sender == self.sender)
    }
}
