// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use ethers_core::types::U256;
use eventuals::{timer, Eventual, EventualExt};
use serde::Deserialize;
use thegraph::types::Address;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, warn};

use crate::prelude::{Query, SubgraphClient};

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

pub fn escrow_accounts(
    escrow_subgraph: &'static SubgraphClient,
    indexer_address: Address,
    interval: Duration,
    reject_thawing_signers: bool,
) -> Eventual<EscrowAccounts> {
    // Types for deserializing the network subgraph response
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EscrowAccountsResponse {
        escrow_accounts: Vec<EscrowAccount>,
    }
    // Note that U256's serde implementation is based on serializing the internal bytes, not the string decimal
    // representation. This is why we deserialize them as strings below.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EscrowAccount {
        balance: String,
        total_amount_thawing: String,
        sender: Sender,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sender {
        id: Address,
        signers: Vec<Signer>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Signer {
        id: Address,
    }

    // thawEndTimestamp == 0 means that the signer is not thawing. This also means
    // that we don't wait for the thawing period to end before stopping serving
    // queries for this signer.
    // isAuthorized == true means that the signer is still authorized to sign
    // payments in the name of the sender.
    let query = if reject_thawing_signers {
        r#"
        query ($indexer: ID!) {
            escrowAccounts(where: {receiver_: {id: $indexer}}) {
                balance
                totalAmountThawing
                sender {
                    id
                    signers(
                        where: {thawEndTimestamp: "0", isAuthorized: true}
                    ) {
                        id
                    }
                }
            }
        }
    "#
    } else {
        r#"
        query ($indexer: ID!) {
            escrowAccounts(where: {receiver_: {id: $indexer}}) {
                balance
                totalAmountThawing
                sender {
                    id
                    signers(
                        where: {isAuthorized: true}
                    ) {
                        id
                    }
                }
            }
        }
    "#
    };

    timer(interval).map_with_retry(
        move |_| async move {
            let response = escrow_subgraph
                .query::<EscrowAccountsResponse>(Query::new_with_variables(
                    query,
                    [("indexer", format!("{:x?}", indexer_address).into())],
                ))
                .await
                .map_err(|e| e.to_string())?;

            let response = response.map_err(|e| e.to_string())?;

            let senders_balances = response
                .escrow_accounts
                .iter()
                .map(|account| {
                    let balance = U256::checked_sub(
                        U256::from_dec_str(&account.balance)?,
                        U256::from_dec_str(&account.total_amount_thawing)?,
                    )
                    .unwrap_or_else(|| {
                        warn!(
                            "Balance minus total amount thawing underflowed for account {}. \
                                 Setting balance to 0, no queries will be served for this sender.",
                            account.sender.id
                        );
                        U256::from(0)
                    });

                    Ok((account.sender.id, balance))
                })
                .collect::<Result<HashMap<_, _>, anyhow::Error>>()
                .map_err(|e| format!("{}", e))?;

            let senders_to_signers = response
                .escrow_accounts
                .iter()
                .map(|account| {
                    let sender = account.sender.id;
                    let signers = account
                        .sender
                        .signers
                        .iter()
                        .map(|signer| signer.id)
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
            DeploymentDetails::for_query_url(
                &format!(
                    "{}/subgraphs/id/{}",
                    &mock_server.uri(),
                    *test_vectors::ESCROW_SUBGRAPH_DEPLOYMENT
                ),
                None,
            )
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
