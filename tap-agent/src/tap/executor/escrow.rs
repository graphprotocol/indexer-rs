// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use tap_core::manager::adapters::EscrowHandler as EscrowAdapterTrait;
use thegraph::types::Address;

use super::{error::AdapterError, TapAgentExecutor};

// Conversion from eventuals::error::Closed to AdapterError::EscrowEventualError
impl From<eventuals::error::Closed> for AdapterError {
    fn from(e: eventuals::error::Closed) -> Self {
        AdapterError::EscrowEventualError {
            error: format!("{:?}", e),
        }
    }
}

#[async_trait]
impl EscrowAdapterTrait for TapAgentExecutor {
    type AdapterError = AdapterError;

    async fn get_available_escrow(&self, sender: Address) -> Result<u128, AdapterError> {
        self.escrow_adapter.get_available_escrow(sender).await
    }

    async fn subtract_escrow(&self, sender: Address, value: u128) -> Result<(), AdapterError> {
        self.escrow_adapter.subtract_escrow(sender, value).await
    }

    async fn verify_signer(&self, sender: Address) -> Result<bool, Self::AdapterError> {
        self.escrow_adapter.verify_signer(sender).await
    }
}
