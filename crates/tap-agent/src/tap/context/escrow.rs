// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use tap_core::manager::adapters::EscrowHandler as EscrowAdapterTrait;
use thegraph_core::alloy::primitives::Address;

use super::{error::AdapterError, TapAgentContext};

// Conversion from eventuals::error::Closed to AdapterError::EscrowEventualError
impl From<eventuals::error::Closed> for AdapterError {
    fn from(e: eventuals::error::Closed) -> Self {
        AdapterError::EscrowEventualError {
            error: format!("{:?}", e),
        }
    }
}

// we don't need these checks anymore because there are being done before triggering
// a rav request.
//
// In any case, we don't want to fail a receipt because of this.
// The receipt is fine, just the escrow account that is not.
#[async_trait]
impl EscrowAdapterTrait for TapAgentContext {
    type AdapterError = AdapterError;

    async fn get_available_escrow(&self, _signer: Address) -> Result<u128, AdapterError> {
        Ok(0)
    }

    async fn subtract_escrow(&self, _signer: Address, _value: u128) -> Result<(), AdapterError> {
        Ok(())
    }

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
