// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use anyhow::anyhow;
use indexer_query::escrow_account::{self, EscrowAccountQuery};
use indexer_query::escrow_account_v2::{
    self as escrow_account_v2, EscrowAccountQuery as EscrowAccountQueryV2,
};
use thegraph_core::alloy::primitives::{Address, U256};
use thiserror::Error;
use tokio::sync::watch::Receiver;

use crate::client::SubgraphClient;

#[derive(Error, Debug)]
pub enum EscrowAccountsError {
    #[error("No signer found for sender {sender}")]
    NoSignerFound { sender: Address },
    #[error("No balance found for sender {sender}")]
    NoBalanceFound { sender: Address },
    #[error("No sender found for signer {signer}")]
    NoSenderFound { signer: Address },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EscrowAccounts {
    senders_balances: HashMap<Address, U256>,
    signers_to_senders: HashMap<Address, Address>,
    senders_to_signers: HashMap<Address, Vec<Address>>,
}

impl EscrowAccounts {
    pub fn new(
        senders_balances: HashMap<Address, U256>,
        senders_to_signers: HashMap<Address, Vec<Address>>,
    ) -> Self {
        let signers_to_senders = senders_to_signers
            .iter()
            .flat_map(|(sender, signers)| signers.iter().map(move |signer| (*signer, *sender)))
            .collect();

        Self {
            senders_balances,
            signers_to_senders,
            senders_to_signers,
        }
    }

    pub fn get_signers_for_sender(&self, sender: &Address) -> Vec<Address> {
        self.senders_to_signers
            .get(sender)
            .filter(|signers| !signers.is_empty())
            .map(|signers| signers.to_owned())
            // if none, just return an empty vec
            .unwrap_or_default()
    }

    pub fn get_sender_for_signer(&self, signer: &Address) -> Result<Address, EscrowAccountsError> {
        self.signers_to_senders
            .get(signer)
            .ok_or(EscrowAccountsError::NoSenderFound {
                signer: signer.to_owned(),
            })
            .copied()
    }

    pub fn get_balance_for_sender(&self, sender: &Address) -> Result<U256, EscrowAccountsError> {
        self.senders_balances
            .get(sender)
            .ok_or(EscrowAccountsError::NoBalanceFound {
                sender: sender.to_owned(),
            })
            .copied()
    }

    pub fn get_balance_for_signer(&self, signer: &Address) -> Result<U256, EscrowAccountsError> {
        self.get_sender_for_signer(signer)
            .and_then(|sender| self.get_balance_for_sender(&sender))
    }

    pub fn get_senders(&self) -> HashSet<Address> {
        self.senders_balances.keys().copied().collect()
    }
}

pub type EscrowAccountsWatcher = Receiver<EscrowAccounts>;

pub async fn escrow_accounts_v1(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    reject_thawing_signers: bool,
) -> Result<EscrowAccountsWatcher, anyhow::Error> {
    indexer_watcher::new_watcher(interval, move || {
        get_escrow_accounts_v1(escrow_subgraph, indexer_address, reject_thawing_signers)
    })
    .await
}

pub async fn escrow_accounts_v2(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    reject_thawing_signers: bool,
) -> Result<EscrowAccountsWatcher, anyhow::Error> {
    indexer_watcher::new_watcher(interval, move || {
        get_escrow_accounts_v2(escrow_subgraph, indexer_address, reject_thawing_signers)
    })
    .await
}

async fn get_escrow_accounts_v2(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    reject_thawing_signers: bool,
) -> anyhow::Result<EscrowAccounts> {
    // V2 TAP receipts use different field names (payer/service_provider) but the underlying
    // escrow account model is identical to V1. Both V1 and V2 receipts reference the same
    // sender addresses and the same escrow relationships.
    //
    // The separation of V1/V2 escrow account watchers allows for potential future differences
    // in escrow models, but currently both query the same subgraph data with identical logic.
    //
    // V2 receipt flow:
    // 1. V2 receipt contains payer address (equivalent to V1 sender)
    // 2. Receipt is signed by a signer authorized by the payer
    // 3. Escrow accounts map: signer -> payer (sender) -> balance
    // 4. Service provider (indexer) receives payments from payer's escrow

    let response = escrow_subgraph
        .query::<EscrowAccountQueryV2, _>(escrow_account_v2::Variables {
            indexer: format!("{:x?}", indexer_address),
            thaw_end_timestamp: if reject_thawing_signers {
                U256::ZERO.to_string()
            } else {
                U256::MAX.to_string()
            },
        })
        .await?;

    let response = response?;

    tracing::trace!("V2 Escrow accounts response: {:?}", response);

    let senders_balances: HashMap<Address, U256> = response
        .escrow_accounts
        .iter()
        .map(|account| {
            let balance = U256::checked_sub(
                U256::from_str(&account.balance)?,
                U256::from_str(&account.total_amount_thawing)?,
            )
            .unwrap_or_else(|| {
                tracing::warn!(
                    "Balance minus total amount thawing underflowed for V2 account {}. \
                                 Setting balance to 0, no V2 queries will be served for this payer.",
                    account.payer.id
                );
                U256::from(0)
            });

            Ok((Address::from_str(&account.payer.id)?, balance))
        })
        .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

    let senders_to_signers = response
        .escrow_accounts
        .into_iter()
        .map(|account| {
            let payer = Address::from_str(&account.payer.id)?;
            let signers = account
                .payer
                .signers
                .iter()
                .map(|signer| Address::from_str(&signer.id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((payer, signers))
        })
        .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

    Ok(EscrowAccounts::new(senders_balances, senders_to_signers))
}

async fn get_escrow_accounts_v1(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    reject_thawing_signers: bool,
) -> anyhow::Result<EscrowAccounts> {
    // thawEndTimestamp == 0 means that the signer is not thawing. This also means
    // that we don't wait for the thawing period to end before stopping serving
    // queries for this signer.
    // isAuthorized == true means that the signer is still authorized to sign
    // payments in the name of the sender.
    let response = escrow_subgraph
        .query::<EscrowAccountQuery, _>(escrow_account::Variables {
            indexer: format!("{:x?}", indexer_address),
            thaw_end_timestamp: if reject_thawing_signers {
                U256::ZERO.to_string()
            } else {
                U256::MAX.to_string()
            },
        })
        .await?;

    let response = response?;

    tracing::trace!("Escrow accounts response: {:?}", response);

    let senders_balances: HashMap<Address, U256> = response
        .escrow_accounts
        .iter()
        .map(|account| {
            let balance = U256::checked_sub(
                U256::from_str(&account.balance)?,
                U256::from_str(&account.total_amount_thawing)?,
            )
            .unwrap_or_else(|| {
                tracing::warn!(
                    "Balance minus total amount thawing underflowed for account {}. \
                                 Setting balance to 0, no queries will be served for this sender.",
                    account.sender.id
                );
                U256::from(0)
            });

            Ok((Address::from_str(&account.sender.id)?, balance))
        })
        .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

    let senders_to_signers = response
        .escrow_accounts
        .into_iter()
        .map(|account| {
            let sender = Address::from_str(&account.sender.id)?;
            let signers = account
                .sender
                .signers
                .ok_or(anyhow!("Could not find any signers for sender {sender}"))?
                .iter()
                .map(|signer| Address::from_str(&signer.id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((sender, signers))
        })
        .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

    Ok(EscrowAccounts::new(senders_balances, senders_to_signers))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use test_assets::{
        ESCROW_ACCOUNTS_BALANCES, ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS,
        ESCROW_ACCOUNTS_SIGNERS_TO_SENDERS,
    };
    use test_log::test;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::client::{DeploymentDetails, SubgraphClient};

    #[test]
    fn test_new_escrow_accounts() {
        let escrow_accounts = EscrowAccounts::new(
            ESCROW_ACCOUNTS_BALANCES.to_owned(),
            ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        );

        assert_eq!(
            escrow_accounts.signers_to_senders,
            ESCROW_ACCOUNTS_SIGNERS_TO_SENDERS.to_owned()
        )
    }

    #[test(tokio::test)]
    async fn test_current_accounts() {
        // Set up a mock escrow subgraph
        let mock_server = MockServer::start().await;
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&format!(
                    "{}/subgraphs/id/{}",
                    &mock_server.uri(),
                    test_assets::ESCROW_SUBGRAPH_DEPLOYMENT
                ))
                .unwrap(),
            )
            .await,
        ));

        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                test_assets::ESCROW_SUBGRAPH_DEPLOYMENT
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_assets::ESCROW_QUERY_RESPONSE, "application/json"),
            );
        mock_server.register(mock).await;

        let mut accounts = escrow_accounts_v1(
            escrow_subgraph,
            test_assets::INDEXER_ADDRESS,
            Duration::from_secs(60),
            true,
        )
        .await
        .unwrap();
        accounts.changed().await.unwrap();
        assert_eq!(
            accounts.borrow().clone(),
            EscrowAccounts::new(
                ESCROW_ACCOUNTS_BALANCES.to_owned(),
                ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
            )
        );
    }

    #[test(tokio::test)]
    async fn test_current_accounts_v2() {
        // Set up a mock escrow subgraph - V2 uses the same subgraph as V1
        let mock_server = MockServer::start().await;
        let escrow_subgraph = Box::leak(Box::new(
            SubgraphClient::new(
                reqwest::Client::new(),
                None,
                DeploymentDetails::for_query_url(&format!(
                    "{}/subgraphs/id/{}",
                    &mock_server.uri(),
                    test_assets::ESCROW_SUBGRAPH_DEPLOYMENT
                ))
                .unwrap(),
            )
            .await,
        ));

        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                test_assets::ESCROW_SUBGRAPH_DEPLOYMENT
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_assets::ESCROW_QUERY_RESPONSE, "application/json"),
            );
        mock_server.register(mock).await;

        // Test V2 escrow accounts watcher
        let mut accounts = escrow_accounts_v2(
            escrow_subgraph,
            test_assets::INDEXER_ADDRESS,
            Duration::from_secs(60),
            true,
        )
        .await
        .unwrap();
        accounts.changed().await.unwrap();

        // V2 should produce identical results to V1 since they query the same data
        assert_eq!(
            accounts.borrow().clone(),
            EscrowAccounts::new(
                ESCROW_ACCOUNTS_BALANCES.to_owned(),
                ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
            )
        );
    }
}
