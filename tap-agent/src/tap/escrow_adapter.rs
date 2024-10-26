// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, RwLock};

use alloy::primitives::Address;
use async_trait::async_trait;
use indexer_common::escrow_accounts::EscrowAccounts;
use tap_core::manager::adapters::EscrowHandler as EscrowAdapterTrait;
use tokio::sync::watch::Receiver;

use super::context::AdapterError;

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
    escrow_accounts: Receiver<EscrowAccounts>,
    sender_id: Address,
    sender_pending_fees: Arc<RwLock<u128>>,
}

impl EscrowAdapter {
    pub fn new(escrow_accounts: Receiver<EscrowAccounts>, sender_id: Address) -> Self {
        Self {
            escrow_accounts,
            sender_pending_fees: Arc::new(RwLock::new(0)),
            sender_id,
        }
    }
}

#[async_trait]
impl EscrowAdapterTrait for EscrowAdapter {
    type AdapterError = AdapterError;

    async fn get_available_escrow(&self, signer: Address) -> Result<u128, AdapterError> {
        let escrow_accounts = self.escrow_accounts.borrow();

        let sender = escrow_accounts.get_sender_for_signer(&signer)?;

        let balance = escrow_accounts.get_balance_for_sender(&sender)?.to_owned();
        let balance: u128 = balance
            .try_into()
            .map_err(|_| AdapterError::BalanceTooLarge {
                sender: sender.to_owned(),
            })?;

        let fees = *self.sender_pending_fees.read().unwrap();
        Ok(balance - fees)
    }

    async fn subtract_escrow(&self, signer: Address, value: u128) -> Result<(), AdapterError> {
        //  change cloning seems essential
        let escrow_accounts = self.escrow_accounts.borrow().clone();

        let current_available_escrow = self.get_available_escrow(signer).await?;

        let sender = escrow_accounts.get_sender_for_signer(&signer)?;

        let mut fees = self.sender_pending_fees.write().unwrap();
        if current_available_escrow < value {
            return Err(AdapterError::NotEnoughEscrow {
                sender: sender.to_owned(),
                fees: value,
                balance: current_available_escrow,
            });
        }
        *fees += value;
        Ok(())
    }

    async fn verify_signer(&self, signer: Address) -> Result<bool, Self::AdapterError> {
        let escrow_accounts = self.escrow_accounts.borrow();
        let sender = escrow_accounts.get_sender_for_signer(&signer).map_err(|_| {
            AdapterError::ValidationError {
                error: format!("Could not find the sender for the signer {}", signer),
            }
        })?;
        Ok(sender == self.sender_id)
    }
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, vec};

    use alloy::primitives::U256;
    use tokio::sync::watch;

    use crate::tap::test_utils::{SENDER, SIGNER};

    use super::*;

    impl super::EscrowAdapter {
        pub fn mock() -> Self {
            let escrow_accounts = watch::channel(EscrowAccounts::new(
                HashMap::from([(SENDER.1, U256::from(1000))]),
                HashMap::from([(SENDER.1, vec![SIGNER.1])]),
            )).1;
            Self {
                escrow_accounts,
                sender_pending_fees: Arc::new(RwLock::new(0)),
                sender_id: Address::ZERO,
            }
        }
    }

    #[tokio::test]
    async fn test_subtract_escrow() {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        )).1;

        let sender_pending_fees = Arc::new(RwLock::new(500));

        let adapter = EscrowAdapter {
            escrow_accounts,
            sender_pending_fees,
            sender_id: Address::ZERO,
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
        let escrow_accounts =  watch::channel(EscrowAccounts::new(
            HashMap::from([(SENDER.1, U256::from(1000))]),
            HashMap::from([(SENDER.1, vec![SIGNER.1])]),
        )).1;

        let sender_pending_fees = Arc::new(RwLock::new(500));

        let adapter = EscrowAdapter {
            escrow_accounts,
            sender_pending_fees,
            sender_id: Address::ZERO,
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
