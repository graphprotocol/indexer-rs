// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use super::monitor::{monitor_deployment_status, DeploymentStatus};
use anyhow::anyhow;
use axum::body::Bytes;
use graphql_client::GraphQLQuery;
use reqwest::{header, Url};
use thegraph_core::DeploymentId;
use tokio::sync::watch::Receiver;
use tracing::warn;

pub type ResponseResult<T> = Result<T, anyhow::Error>;

#[derive(Debug, Clone)]
pub struct DeploymentDetails {
    deployment: Option<DeploymentId>,
    status_url: Option<Url>,
    query_url: Url,
    query_auth_token: Option<String>,
}

impl DeploymentDetails {
    pub fn for_graph_node(
        graph_node_status_url: &str,
        graph_node_base_url: &str,
        deployment: DeploymentId,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self::for_graph_node_url(
            Url::parse(graph_node_status_url)?,
            Url::parse(graph_node_base_url)?,
            deployment,
        ))
    }

    pub fn for_graph_node_url(
        graph_node_status_url: Url,
        graph_node_base_url: Url,
        deployment: DeploymentId,
    ) -> Self {
        Self {
            deployment: Some(deployment),
            status_url: Some(graph_node_status_url),
            query_url: graph_node_base_url
                .join(&format!("subgraphs/id/{deployment}"))
                .expect("Must be correct"),
            query_auth_token: None,
        }
    }

    pub fn for_query_url(query_url: &str) -> Result<Self, anyhow::Error> {
        Ok(Self {
            deployment: None,
            status_url: None,
            query_url: Url::parse(query_url)?,
            query_auth_token: None,
        })
    }

    pub fn for_query_url_with_token(query_url: Url, query_auth_token: Option<String>) -> Self {
        Self {
            deployment: None,
            status_url: None,
            query_url,
            query_auth_token,
        }
    }
}

struct DeploymentClient {
    pub http_client: reqwest::Client,
    pub status: Option<Receiver<DeploymentStatus>>,
    pub query_url: Url,
    pub query_auth_token: Option<String>,
}

impl DeploymentClient {
    pub async fn new(http_client: reqwest::Client, details: DeploymentDetails) -> Self {
        Self {
            http_client,
            status: match details.deployment.zip(details.status_url) {
                Some((deployment, url)) => Some(
                    monitor_deployment_status(deployment, url)
                        .await
                        .unwrap_or_else(|_| {
                            panic!(
                                "Failed to initialize monitoring for deployment `{}`",
                                deployment
                            )
                        }),
                ),
                None => None,
            },
            query_url: details.query_url,
            query_auth_token: details.query_auth_token,
        }
    }

    pub async fn query<T: GraphQLQuery>(
        &self,
        variables: T::Variables,
    ) -> Result<ResponseResult<T::ResponseData>, anyhow::Error> {
        if let Some(ref status) = self.status {
            let deployment_status = status.borrow();

            if !deployment_status.synced || &deployment_status.health != "healthy" {
                return Err(anyhow!(
                    "Deployment `{}` is not ready or healthy to be queried",
                    self.query_url
                ));
            }
        }

        let body = T::build_query(variables);
        let mut req = self
            .http_client
            .post(self.query_url.as_ref())
            .header(header::USER_AGENT, "indexer-common")
            .json(&body);

        if let Some(token) = self.query_auth_token.as_ref() {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }

        let reqwest_response = req.send().await?;
        let response: graphql_client::Response<T::ResponseData> = reqwest_response.json().await?;

        // TODO handle partial responses
        Ok(match (response.data, response.errors) {
            (Some(data), None) => Ok(data),
            (None, Some(errors)) => Err(anyhow!("{errors:?}")),
            (Some(_data), Some(err)) => Err(anyhow!("Unsupported partial results. Error: {err:?}")),
            (None, None) => {
                let body = serde_json::to_string(&body).unwrap_or_default();
                Err(anyhow!(
                    "No data or error returned for query: {body}. Endpoint: {}",
                    self.query_url.as_str()
                ))
            }
        })
    }

    pub async fn query_raw(&self, body: Bytes) -> Result<reqwest::Response, anyhow::Error> {
        if let Some(ref status) = self.status {
            let deployment_status = status.borrow();

            if !deployment_status.synced || &deployment_status.health != "healthy" {
                return Err(anyhow!(
                    "Deployment `{}` is not ready or healthy to be queried",
                    self.query_url
                ));
            }
        }

        let mut req = self
            .http_client
            .post(self.query_url.as_ref())
            .header(header::USER_AGENT, "indexer-common")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body);

        if let Some(token) = self.query_auth_token.as_ref() {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }

        Ok(req.send().await?)
    }
}

/// Client for a subgraph that can fall back from a local deployment to a remote query URL
pub struct SubgraphClient {
    local_client: Option<DeploymentClient>,
    remote_client: DeploymentClient,
}

impl SubgraphClient {
    pub async fn new(
        http_client: reqwest::Client,
        local_deployment: Option<DeploymentDetails>,
        remote_deployment: DeploymentDetails,
    ) -> Self {
        Self {
            local_client: match local_deployment {
                Some(d) => Some(DeploymentClient::new(http_client.clone(), d).await),
                None => None,
            },
            remote_client: DeploymentClient::new(http_client, remote_deployment).await,
        }
    }

    pub async fn query<Q, V>(
        &self,
        variables: Q::Variables,
    ) -> Result<ResponseResult<Q::ResponseData>, anyhow::Error>
    where
        Q: GraphQLQuery<Variables = V>,
        V: Clone,
    {
        // Try the local client first; if that fails, log the error and move on
        // to the remote client
        if let Some(ref local_client) = self.local_client {
            match local_client.query::<Q>(variables.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) => warn!(
                    "Failed to query local subgraph deployment `{}`, trying remote deployment next: {}",
                    local_client.query_url, err
                ),
            }
        }

        // Try the remote client
        self.remote_client
            .query::<Q>(variables)
            .await
            .map_err(|err| {
                warn!(
                    "Failed to query remote subgraph deployment `{}`: {}",
                    self.remote_client.query_url, err
                );

                err
            })
    }

    pub async fn query_raw(&self, query: Bytes) -> Result<reqwest::Response, anyhow::Error> {
        // Try the local client first; if that fails, log the error and move on
        // to the remote client
        if let Some(ref local_client) = self.local_client {
            match local_client.query_raw(query.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) => warn!(
                    "Failed to query local subgraph deployment `{}`, trying remote deployment next: {}",
                    local_client.query_url, err
                ),
            }
        }

        // Try the remote client
        self.remote_client.query_raw(query).await.map_err(|err| {
            warn!(
                "Failed to query remote subgraph deployment `{}`: {}",
                self.remote_client.query_url, err
            );

            err
        })
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use indexer_query::{current_epoch, user_query, CurrentEpoch, UserQuery};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const NETWORK_SUBGRAPH_URL: &str =
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli";

    async fn mock_graph_node_server() -> MockServer {
        let mock_server = MockServer::start().await;
        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                *test_assets::NETWORK_SUBGRAPH_DEPLOYMENT
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"
                {
                    "data": {
                        "graphNetwork": {
                            "currentEpoch": 960
                        }
                    }
                }
                "#,
                "application/json",
            ));
        mock_server.register(mock).await;

        mock_server
    }

    async fn network_subgraph_client() -> SubgraphClient {
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(NETWORK_SUBGRAPH_URL).unwrap(),
        )
        .await
    }

    #[tokio::test]
    #[ignore = "depends on the defunct hosted-service"]
    async fn test_network_query() {
        let _mock_server = mock_graph_node_server().await;

        // Check that the response is valid JSON
        let result = network_subgraph_client()
            .await
            .query::<CurrentEpoch, _>(current_epoch::Variables {})
            .await
            .unwrap();

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_uses_local_deployment_if_healthy_and_synced() {
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        let mock_server_status = MockServer::start().await;
        mock_server_status
            .register(Mock::given(method("POST")).respond_with(
                ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "indexingStatuses": [
                            {
                                "synced": true,
                                "health": "healthy"
                            }
                        ]
                    }
                })),
            ))
            .await;

        let mock_server_local = MockServer::start().await;
        mock_server_local
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "local"
                            }
                        }
                    }))),
            )
            .await;

        let mock_server_remote = MockServer::start().await;
        mock_server_remote
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "remote"
                            }
                        }
                    }))),
            )
            .await;

        // Create the subgraph client
        let client = SubgraphClient::new(
            reqwest::Client::new(),
            Some(
                DeploymentDetails::for_graph_node(
                    &mock_server_status.uri(),
                    &mock_server_local.uri(),
                    deployment,
                )
                .unwrap(),
            ),
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                mock_server_remote.uri(),
                deployment
            ))
            .unwrap(),
        );

        // Query the subgraph
        let data = client
            .await
            .query::<UserQuery, _>(user_query::Variables {})
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data.user.name, "local".to_string());
    }

    #[tokio::test]
    async fn test_uses_query_url_if_local_deployment_is_unhealthy() {
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        let mock_server_status = MockServer::start().await;
        mock_server_status
            .register(Mock::given(method("POST")).respond_with(
                ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "indexingStatuses": [
                            {
                                "synced": true,
                                "health": "unhealthy"
                            }
                        ]
                    }
                })),
            ))
            .await;

        let mock_server_local = MockServer::start().await;
        mock_server_local
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "local"
                            }
                        }
                    }))),
            )
            .await;

        let mock_server_remote = MockServer::start().await;
        mock_server_remote
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "remote"
                            }
                        }
                    }))),
            )
            .await;

        // Create the subgraph client
        let client = SubgraphClient::new(
            reqwest::Client::new(),
            Some(
                DeploymentDetails::for_graph_node(
                    &mock_server_status.uri(),
                    &mock_server_local.uri(),
                    deployment,
                )
                .unwrap(),
            ),
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                mock_server_remote.uri(),
                deployment
            ))
            .unwrap(),
        );

        // Query the subgraph
        let data = client
            .await
            .query::<UserQuery, _>(user_query::Variables {})
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data.user.name, "remote".to_string());
    }

    #[tokio::test]
    async fn test_uses_query_url_if_local_deployment_is_not_synced() {
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        let mock_server_status = MockServer::start().await;
        mock_server_status
            .register(Mock::given(method("POST")).respond_with(
                ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "indexingStatuses": [
                            {
                                "synced": false,
                                "health": "healthy"
                            }
                        ]
                    }
                })),
            ))
            .await;

        let mock_server_local = MockServer::start().await;
        mock_server_local
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "local"
                            }
                        }
                    }))),
            )
            .await;

        let mock_server_remote = MockServer::start().await;
        mock_server_remote
            .register(
                Mock::given(method("POST"))
                    .and(path(format!("/subgraphs/id/{}", deployment)))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "user": {
                                "name": "remote"
                            }
                        }
                    }))),
            )
            .await;

        // Create the subgraph client
        let client = SubgraphClient::new(
            reqwest::Client::new(),
            Some(
                DeploymentDetails::for_graph_node(
                    &mock_server_status.uri(),
                    &mock_server_local.uri(),
                    deployment,
                )
                .unwrap(),
            ),
            DeploymentDetails::for_query_url(&format!(
                "{}/subgraphs/id/{}",
                mock_server_remote.uri(),
                deployment
            ))
            .unwrap(),
        );

        // Query the subgraph
        let data = client
            .await
            .query::<UserQuery, _>(user_query::Variables {})
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data.user.name, "remote".to_string());
    }
}
