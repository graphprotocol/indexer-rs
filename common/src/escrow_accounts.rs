// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, time::Duration};

use alloy_primitives::Address;
use anyhow::Result;
use ethers_core::types::U256;
use eventuals::{timer, Eventual, EventualExt};
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{error, warn};

use crate::prelude::{Query, SubgraphClient};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EscrowAccounts {
    pub senders_balances: HashMap<Address, U256>,
    pub signers_to_senders: HashMap<Address, Address>,
    pub senders_to_signers: HashMap<Address, Vec<Address>>,
}

impl EscrowAccounts {
    pub fn new(
        senders_balances: HashMap<Address, U256>,
        senders_to_signers: HashMap<Address, Vec<Address>>,
    ) -> Self {
        let signers_to_senders = senders_to_signers
            .iter()
            .flat_map(|(sender, signers)| {
                signers
                    .iter()
                    .map(move |signer| (*signer, *sender))
                    .collect::<Vec<_>>()
            })
            .collect();

        Self {
            senders_balances,
            signers_to_senders,
            senders_to_signers,
        }
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
        authorized_signers: Vec<AuthorizedSigner>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AuthorizedSigner {
        id: Address,
    }

    // thawEndTimestamp == 0 means that the signer is not thawing. This also means
    // that we don't wait for the thawing period to end before stopping serving
    // queries for this signer.
    // isAuthorized == true means that the signer is still authorized to sign
    // payments in the name of the sender.
    let query_no_thawing_signers = r#"
        query ($indexer: ID!) {
            escrowAccounts(where: {receiver_: {id: $indexer}}) {
                balance
                totalAmountThawing
                sender {
                    id
                    authorizedSigners(
                        where: {thawEndTimestamp: "0", isAuthorized: true}
                    ) {
                        id
                    }
                }
            }
        }
    "#;

    let query_with_thawing_signers = r#"
        query ($indexer: ID!) {
            escrowAccounts(where: {receiver_: {id: $indexer}}) {
                balance
                totalAmountThawing
                sender {
                    id
                    authorizedSigners(
                        where: {isAuthorized: true}
                    ) {
                        id
                    }
                }
            }
        }
    "#;

    let query = if reject_thawing_signers {
        query_no_thawing_signers
    } else {
        query_with_thawing_signers
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
                        .authorized_signers
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
