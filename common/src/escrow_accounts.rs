// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Duration,
};

use anyhow::Result;
use ethers_core::types::U256;
use eventuals::{timer, Eventual, EventualExt};
use graphql_client::GraphQLQuery;
use thegraph::types::Address;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, warn};

use crate::prelude::SubgraphClient;

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

type BigInt = U256;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "../graphql/tap.schema.graphql",
    query_path = "../graphql/escrow_account.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct EscrowAccountQuery;

pub fn escrow_accounts(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    reject_thawing_signers: bool,
) -> Eventual<EscrowAccounts> {
    timer(interval).map_with_retry(
        move |_| async move {
            // thawEndTimestamp == 0 means that the signer is not thawing. This also means
            // that we don't wait for the thawing period to end before stopping serving
            // queries for this signer.
            // isAuthorized == true means that the signer is still authorized to sign
            // payments in the name of the sender.
            let response = escrow_subgraph
                .query::<EscrowAccountQuery, _>(escrow_account_query::Variables {
                    indexer: format!("{:x?}", indexer_address),
                    thaw_end_timestamp: if reject_thawing_signers {
                        Some("0".into())
                    } else {
                        None
                    },
                })
                .await
                .map_err(|e| e.to_string())?;

            let response = response.unwrap();

            let senders_balances: HashMap<Address, U256> = response
                .escrow_accounts
                .iter()
                .map(|account| {
                    let balance = U256::checked_sub(account.balance, account.total_amount_thawing)
                        .unwrap_or_else(|| {
                            warn!(
                                "Balance minus total amount thawing underflowed for account {}. \
                                 Setting balance to 0, no queries will be served for this sender.",
                                account.sender.id
                            );
                            U256::from(0)
                        });

                    Ok((Address::from_str(&account.sender.id).unwrap(), balance))
                })
                .collect::<Result<HashMap<_, _>, anyhow::Error>>()
                .map_err(|e| format!("{}", e))?;

            let senders_to_signers = response
                .escrow_accounts
                .iter()
                .map(|account| {
                    let sender = Address::from_str(&account.sender.id).unwrap();
                    let signers = account
                        .sender
                        .signers
                        .as_ref()
                        .unwrap()
                        .iter()
                        .map(|signer| Address::from_str(&signer.id).unwrap())
                        .collect();
                    (sender, signers)
                })
                .collect();

            Ok(EscrowAccounts::new(senders_balances, senders_to_signers))
        },
        move |err: String| {
            error!(
                "Failed to fetch escrow accounts for indexer {:?}: {}",
                indexer_address, err
            );

            sleep(interval.div_f32(2.0))
        },
    )
}

#[cfg(test)]
mod tests {
    use test_log::test;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::prelude::DeploymentDetails;
    use crate::test_vectors;

    use super::*;

    #[test]
    fn test_new_escrow_accounts() {
        let escrow_accounts = EscrowAccounts::new(
            test_vectors::ESCROW_ACCOUNTS_BALANCES.to_owned(),
            test_vectors::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        );

        assert_eq!(
            escrow_accounts.signers_to_senders,
            test_vectors::ESCROW_ACCOUNTS_SIGNERS_TO_SENDERS.to_owned()
        )
    }

    #[test(tokio::test)]
    async fn test_current_accounts() {
        // Set up a mock escrow subgraph
        let mock_server = MockServer::start().await;
        let escrow_subgraph = Box::leak(Box::new(SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                &mock_server.uri(),
                *test_vectors::ESCROW_SUBGRAPH_DEPLOYMENT
            ))
            .unwrap(),
        )));

        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                *test_vectors::ESCROW_SUBGRAPH_DEPLOYMENT
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(test_vectors::ESCROW_QUERY_RESPONSE, "application/json"),
            );
        mock_server.register(mock).await;

        let accounts = escrow_accounts(
            escrow_subgraph,
            *test_vectors::INDEXER_ADDRESS,
            Duration::from_secs(60),
            true,
        );

        assert_eq!(
            accounts.value().await.unwrap(),
            EscrowAccounts::new(
                test_vectors::ESCROW_ACCOUNTS_BALANCES.to_owned(),
                test_vectors::ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
            )
        );
    }
}
