// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use eventuals::Eventual;
use graphql_http::http::response::ResponseBody;
use reqwest::{header, Url};
use serde::de::Deserialize;
use serde_json::Value;
use thegraph::types::DeploymentId;
use tracing::warn;

use super::monitor::{monitor_deployment_status, DeploymentStatus};

#[derive(Debug, Clone)]
pub struct DeploymentDetails {
    pub deployment: Option<DeploymentId>,
    pub status_url: Option<Url>,
    pub query_url: Url,
}

impl DeploymentDetails {
    pub fn for_graph_node(
        graph_node_status_url: &str,
        graph_node_base_url: &str,
        deployment: DeploymentId,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self {
            deployment: Some(deployment),
            status_url: Some(Url::parse(graph_node_status_url)?),
            query_url: Url::parse(&format!("{graph_node_base_url}/subgraphs/id/{deployment}"))?,
        })
    }

    pub fn for_query_url(query_url: &str) -> Result<Self, anyhow::Error> {
        Ok(Self {
            deployment: None,
            status_url: None,
            query_url: Url::parse(query_url)?,
        })
    }
}

struct DeploymentClient {
    pub http_client: reqwest::Client,
    pub status: Option<Eventual<DeploymentStatus>>,
    pub query_url: Url,
}

impl DeploymentClient {
    pub fn new(http_client: reqwest::Client, details: DeploymentDetails) -> Self {
        Self {
            http_client,
            status: details
                .deployment
                .zip(details.status_url)
                .map(|(deployment, url)| monitor_deployment_status(deployment, url)),
            query_url: details.query_url,
        }
    }

    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        body: &Value,
    ) -> Result<ResponseBody<T>, anyhow::Error> {
        if let Some(ref status) = self.status {
            let deployment_status = status.value().await.expect("reading deployment status");

            if !deployment_status.synced || &deployment_status.health != "healthy" {
                return Err(anyhow!(
                    "Deployment `{}` is not ready or healthy to be queried",
                    self.query_url
                ));
            }
        }

        Ok(self
            .http_client
            .post(self.query_url.as_ref())
            .json(body)
            .header(header::USER_AGENT, "indexer-common")
            .header(header::CONTENT_TYPE, "application/json")
            .send()
            .await
            .and_then(|response| response.error_for_status())?
            .json::<ResponseBody<T>>()
            .await?)
    }
}

/// Client for a subgraph that can fall back from a local deployment to a remote query URL
pub struct SubgraphClient {
    local_client: Option<DeploymentClient>,
    remote_client: DeploymentClient,
}

impl SubgraphClient {
    pub fn new(
        http_client: reqwest::Client,
        local_deployment: Option<DeploymentDetails>,
        remote_deployment: DeploymentDetails,
    ) -> Self {
        Self {
            local_client: local_deployment.map(|d| DeploymentClient::new(http_client.clone(), d)),
            remote_client: DeploymentClient::new(http_client, remote_deployment),
        }
    }

    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        body: &Value,
    ) -> Result<ResponseBody<T>, anyhow::Error> {
        // Try the local client first; if that fails, log the error and move on
        // to the remote client
        if let Some(ref local_client) = self.local_client {
            match local_client.query(body).await {
                Ok(response) => return Ok(response),
                Err(err) => warn!(
                    "Failed to query local subgraph deployment `{}`, trying remote deployment next: {}",
                    local_client.query_url, err
                ),
            }
        }

        // Try the remote client
        self.remote_client.query(body).await.map_err(|err| {
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

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::test_vectors::{self};

    use super::*;

    const NETWORK_SUBGRAPH_URL: &str =
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli";

    async fn mock_graph_node_server() -> MockServer {
        let mock_server = MockServer::start().await;
        let mock = Mock::given(method("POST"))
            .and(path(format!(
                "/subgraphs/id/{}",
                *test_vectors::NETWORK_SUBGRAPH_DEPLOYMENT
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

    fn network_subgraph_client() -> SubgraphClient {
        SubgraphClient::new(
            reqwest::Client::new(),
            None,
            DeploymentDetails::for_query_url(NETWORK_SUBGRAPH_URL).unwrap(),
        )
    }

    #[tokio::test]
    async fn test_network_query() {
        let _mock_server = mock_graph_node_server().await;

        // Check that the response is valid JSON
        let result = network_subgraph_client()
            .query::<Value>(&json!({
                "query": r#"
                query {
                 	graphNetwork(id: 1) {
                 		currentEpoch
                 	}
                }
            "#,
            }))
            .await
            .unwrap();

        assert!(result.data.is_some());
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
        let response: ResponseBody<Value> = client
            .query(&json!({ "query": "{ user(id: 1} { name } }"}))
            .await
            .unwrap();

        assert_eq!(response.data, Some(json!({ "user": { "name": "local" } })));
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
        let response: ResponseBody<Value> = client
            .query(&json!({ "query": "{ user(id: 1} { name } }"}))
            .await
            .unwrap();

        assert_eq!(response.data, Some(json!({ "user": { "name": "remote" } })));
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
                    .and(path(&format!("/subgraphs/id/{}", deployment)))
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
        let response: ResponseBody<Value> = client
            .query(&json!({ "query": "{ user(id: 1} { name } }"}))
            .await
            .unwrap();

        assert_eq!(response.data, Some(json!({ "user": { "name": "remote" } })));
    }
}
