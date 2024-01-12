// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc};

use alloy_primitives::Address;
use async_trait::async_trait;
use eventuals::Eventual;
use indexer_common::escrow_accounts::EscrowAccounts;
use tap_core::adapters::escrow_adapter::EscrowAdapter as EscrowAdapterTrait;
use thiserror::Error;
use tokio::sync::RwLock;

/// The EscrowAdapter is used to track the available escrow for all senders. It is updated when
/// receipt checks are finalized (right before a RAV request).
///
/// It is to be shared between all Account instances. Note that it is Arc internally, so it can be
/// shared through clones.
///
/// It is not used to track unaggregated fees (yet?), because we are currently batch finalizing
/// receipt checks only when we need to send a RAV request.
#[derive(Clone)]
pub struct EscrowAdapter {
    escrow_accounts: Eventual<EscrowAccounts>,
    sender_pending_fees: Arc<RwLock<HashMap<Address, u128>>>,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Error in EscrowAdapter: {error}")]
    AdapterError { error: String },
}

impl EscrowAdapter {
    pub fn new(escrow_accounts: Eventual<EscrowAccounts>) -> Self {
        Self {
            escrow_accounts,
            sender_pending_fees: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl EscrowAdapterTrait for EscrowAdapter {
    type AdapterError = AdapterError;

    async fn get_available_escrow(&self, sender: Address) -> Result<u128, AdapterError> {
        let escrow_accounts =
            self.escrow_accounts
                .value()
                .await
                .map_err(|e| AdapterError::AdapterError {
                    error: format!("Could not get escrow accounts from eventual: {:?}.", e),
                })?;

        let sender =
            escrow_accounts
                .signers_to_senders
                .get(&sender)
                .ok_or(AdapterError::AdapterError {
                    error: format!(
                        "Sender {} not found for receipt signer, could not get available escrow.",
                        sender
                    )
                    .to_string(),
                })?;

        let balance = escrow_accounts
            .balances
            .get(sender)
            .ok_or(AdapterError::AdapterError {
                error: format!(
                    "Sender {} not found in escrow balances map, could not get available escrow.",
                    sender
                )
                .to_string(),
            })?
            .to_owned();
        let balance: u128 = balance.try_into().map_err(|_| AdapterError::AdapterError {
            error: format!(
                "Sender {} escrow balance is too large to fit in u128, \
            could not get available escrow.",
                sender
            )
            .to_string(),
        })?;

        let fees = self
            .sender_pending_fees
            .read()
            .await
            .get(sender)
            .copied()
            .unwrap_or(0);
        Ok(balance - fees)
    }

    async fn subtract_escrow(&self, sender: Address, value: u128) -> Result<(), AdapterError> {
        let escrow_accounts =
            self.escrow_accounts
                .value()
                .await
                .map_err(|e| AdapterError::AdapterError {
                    error: format!("Could not get escrow accounts from eventual: {:?}.", e),
                })?;

        let current_available_escrow = self.get_available_escrow(sender).await?;

        let sender =
            escrow_accounts
                .signers_to_senders
                .get(&sender)
                .ok_or(AdapterError::AdapterError {
                    error: format!(
                        "Sender {} not found for receipt signer, could not get available escrow.",
                        sender
                    )
                    .to_string(),
                })?;

        let mut fees_write = self.sender_pending_fees.write().await;
        let fees = fees_write.entry(sender.to_owned()).or_insert(0);
        if current_available_escrow < value {
            return Err(AdapterError::AdapterError {
                error: format!(
                    "Sender {} does not have enough escrow to subtract {} from {}.",
                    sender, value, *fees
                )
                .to_string(),
            });
        }
        *fees += value;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::vec;

    use crate::tap::test_utils::{SENDER, SIGNER};

    use super::*;

    #[tokio::test]
    async fn test_subtract_escrow() {
        let escrow_accounts = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let sender_pending_fees = Arc::new(RwLock::new(HashMap::new()));
        sender_pending_fees.write().await.insert(SENDER.1, 500);

        let adapter = EscrowAdapter {
            escrow_accounts,
            sender_pending_fees,
        };
        adapter
            .subtract_escrow(SIGNER.1, 500)
            .await
            .expect("Subtract escrow.");
        let available_escrow = adapter
            .get_available_escrow(SIGNER.1)
            .await
            .expect("Get available escrow.");
        assert_eq!(available_escrow, 0);
    }

    #[tokio::test]
    async fn test_subtract_escrow_overflow() {
        let escrow_accounts = Eventual::from_value(EscrowAccounts::new(
            HashMap::from([(SENDER.1, 1000.into())]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        ));

        let sender_pending_fees = Arc::new(RwLock::new(HashMap::new()));
        sender_pending_fees.write().await.insert(SENDER.1, 500);

        let adapter = EscrowAdapter {
            escrow_accounts,
            sender_pending_fees,
        };
        adapter
            .subtract_escrow(SIGNER.1, 250)
            .await
            .expect("Subtract escrow.");
        assert!(adapter.subtract_escrow(SIGNER.1, 251).await.is_err());
        let available_escrow = adapter
            .get_available_escrow(SIGNER.1)
            .await
            .expect("Get available escrow.");
        assert_eq!(available_escrow, 250);
    }
}
