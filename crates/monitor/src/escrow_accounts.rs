// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use anyhow::anyhow;
use indexer_query::escrow_account::{self, EscrowAccountQuery};
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

    /// Returns the number of signer-to-sender mappings
    pub fn signer_count(&self) -> usize {
        self.signers_to_senders.len()
    }

    pub fn iter_signers_to_senders(&self) -> impl Iterator<Item = (&Address, &Address)> {
        self.signers_to_senders.iter()
    }

    pub fn get_signers(&self) -> HashSet<Address> {
        self.signers_to_senders.keys().copied().collect()
    }

    pub fn contains_signer(&self, signer: &Address) -> bool {
        self.signers_to_senders.contains_key(signer)
    }

    pub fn get_all_mappings(&self) -> Vec<(Address, Address)> {
        self.signers_to_senders
            .iter()
            .map(|(signer, sender)| (*signer, *sender))
            .collect()
    }
}

pub type EscrowAccountsWatcher = Receiver<EscrowAccounts>;

pub fn empty_escrow_accounts_watcher() -> EscrowAccountsWatcher {
    let (_, receiver) =
        tokio::sync::watch::channel(EscrowAccounts::new(HashMap::new(), HashMap::new()));
    receiver
}

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
    graph_tally_collector_address: Address,
) -> Result<EscrowAccountsWatcher, anyhow::Error> {
    indexer_watcher::new_watcher(interval, move || {
        get_escrow_accounts_v2(
            escrow_subgraph,
            indexer_address,
            reject_thawing_signers,
            graph_tally_collector_address,
        )
    })
    .await
}

async fn get_escrow_accounts_v2(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    reject_thawing_signers: bool,
    graph_tally_collector_address: Address,
) -> anyhow::Result<EscrowAccounts> {
    tracing::trace!(
        indexer_address = ?indexer_address,
        reject_thawing_signers,
        "Loading V2 escrow accounts for indexer"
    );

    use indexer_query::network_escrow_account_v2::{
        self as network_escrow_account_v2, Block_height, NetworkEscrowAccountQueryV2,
    };

    let page_size: i64 = 200;
    let mut skip: i64 = 0;
    let mut block_hash: Option<String> = None;
    let mut all_accounts = vec![];

    let thaw_end_timestamp = if reject_thawing_signers {
        U256::ZERO.to_string()
    } else {
        U256::MAX.to_string()
    };

    let mut pages_fetched: u64 = 0;

    loop {
        let response = escrow_subgraph
            .query::<NetworkEscrowAccountQueryV2, _>(network_escrow_account_v2::Variables {
                receiver: format!("{indexer_address:x?}"),
                collector: format!("{graph_tally_collector_address:x?}"),
                thaw_end_timestamp: thaw_end_timestamp.clone(),
                first: page_size,
                skip,
                block: block_hash.as_ref().map(|h| Block_height {
                    hash: Some(h.clone()),
                    number: None,
                    number_gte: None,
                }),
            })
            .await?;

        let page_len = response.payments_escrow_accounts.len();
        pages_fetched += 1;

        // Pin to the block hash from the first page for consistent reads across pages
        if block_hash.is_none() {
            block_hash = response.meta.and_then(|m| m.block.hash);
        }

        // Detect possible signer truncation within any payer on this page
        for account in &response.payments_escrow_accounts {
            if account.payer.signers.len() >= 1000 {
                tracing::warn!(
                    payer = %account.payer.id,
                    signers = account.payer.signers.len(),
                    "Payer signers hit the 1000-per-payer query cap; additional signers may be truncated"
                );
            }
        }

        all_accounts.extend(response.payments_escrow_accounts);

        if (page_len as i64) < page_size {
            break;
        }
        skip += page_size;
    }

    tracing::info!(
        pages = pages_fetched,
        accounts = all_accounts.len(),
        "V2 escrow account pagination complete"
    );

    if pages_fetched > 5 {
        tracing::warn!(
            pages = pages_fetched,
            accounts = all_accounts.len(),
            "Unusually high number of V2 escrow accounts; this may indicate a dust-deposit crowding attack"
        );
    }

    let senders_balances: HashMap<Address, U256> = all_accounts
        .iter()
        .map(|account| {
            let balance = U256::checked_sub(
                U256::from_str(&account.balance)?,
                U256::from_str(&account.total_amount_thawing)?,
            )
            .unwrap_or_else(|| {
                tracing::warn!(
                    payer = ?account.payer.id,
                    "Balance minus total amount thawing underflowed for V2 account; setting balance to 0, no V2 queries will be served for this payer."
                );
                U256::from(0)
            });

            Ok((Address::from_str(&account.payer.id)?, balance))
        })
        .collect::<Result<HashMap<_, _>, anyhow::Error>>()?;

    let senders_to_signers = all_accounts
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

    let escrow_accounts = EscrowAccounts::new(senders_balances, senders_to_signers);

    tracing::info!(
        senders = escrow_accounts.get_senders().len(),
        signers = escrow_accounts.signer_count(),
        "V2 escrow accounts loaded"
    );

    Ok(escrow_accounts)
}

async fn get_escrow_accounts_v1(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    reject_thawing_signers: bool,
) -> anyhow::Result<EscrowAccounts> {
    tracing::debug!(?indexer_address, "Loading V1 escrow accounts for indexer");

    // thawEndTimestamp == 0 means that the signer is not thawing. This also means
    // that we don't wait for the thawing period to end before stopping serving
    // queries for this signer.
    // isAuthorized == true means that the signer is still authorized to sign
    // payments in the name of the sender.
    let response = escrow_subgraph
        .query::<EscrowAccountQuery, _>(escrow_account::Variables {
            indexer: format!("{indexer_address:x?}"),
            thaw_end_timestamp: if reject_thawing_signers {
                U256::ZERO.to_string()
            } else {
                U256::MAX.to_string()
            },
        })
        .await?;

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
                    sender = ?account.sender.id,
                    "Balance minus total amount thawing underflowed for account; setting balance to 0, no queries will be served for this sender."
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

    let escrow_accounts = EscrowAccounts::new(senders_balances.clone(), senders_to_signers.clone());

    tracing::debug!(
        senders = senders_balances.len(),
        mappings = escrow_accounts.signers_to_senders.len(),
        "V1 escrow accounts loaded"
    );

    Ok(escrow_accounts)
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
        // Set up a mock escrow subgraph for V2 with payer fields
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
                    .set_body_raw(test_assets::ESCROW_QUERY_RESPONSE_V2, "application/json"),
            );
        mock_server.register(mock).await;

        // Test V2 escrow accounts watcher
        let mut accounts = escrow_accounts_v2(
            escrow_subgraph,
            test_assets::INDEXER_ADDRESS,
            Duration::from_secs(60),
            true,
            Address::ZERO, // collector address; mock ignores query variables
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
