// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use super::monitor::{monitor_deployment_status, DeploymentStatus};
use anyhow::anyhow;
use axum::body::Bytes;
use eventuals::Eventual;
use reqwest::{header, Url};
use serde::de::Deserialize;
use serde_json::{Map, Value};
use thegraph::types::DeploymentId;
use thegraph_core::client::Client as GraphCoreSubgraphClient;
use thegraph_graphql_http::{
    graphql::{Document, IntoDocument},
    http::request::{IntoRequestParameters, RequestParameters},
};
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Clone)]
pub struct Query {
    pub query: Document,
    pub variables: Map<String, Value>,
}

impl Query {
    pub fn new(query: &str) -> Self {
        Self {
            query: query.into_document(),
            variables: Map::default(),
        }
    }

    pub fn new_with_variables(
        query: impl IntoDocument,
        variables: impl Into<QueryVariables>,
    ) -> Self {
        Self {
            query: query.into_document(),
            variables: variables.into().into(),
        }
    }
}

pub struct QueryVariables(Map<String, Value>);

impl<'a, T> From<T> for QueryVariables
where
    T: IntoIterator<Item = (&'a str, Value)>,
{
    fn from(variables: T) -> Self {
        Self(
            variables
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<Map<_, _>>(),
        )
    }
}

impl From<QueryVariables> for Map<String, Value> {
    fn from(variables: QueryVariables) -> Self {
        variables.0
    }
}

impl IntoRequestParameters for Query {
    fn into_request_parameters(self) -> RequestParameters {
        RequestParameters {
            query: self.query.into_document(),
            variables: self.variables,
            extensions: Map::default(),
            operation_name: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeploymentDetails {
    pub deployment: Option<DeploymentId>,
    pub status_url: Option<Url>,
    pub query_url: Url,
    pub query_auth_token: Option<String>,
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
            query_url: Url::parse(graph_node_base_url)?
                .join(&format!("subgraphs/id/{deployment}"))?,
            query_auth_token: None,
        })
    }

    pub fn for_query_url(query_url: &str) -> Result<Self, anyhow::Error> {
        Ok(Self {
            deployment: None,
            status_url: None,
            query_url: Url::parse(query_url)?,
            query_auth_token: None,
        })
    }

    pub fn for_query_url_with_token(
        query_url: &str,
        query_auth_token: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self {
            deployment: None,
            status_url: None,
            query_url: Url::parse(query_url)?,
            query_auth_token,
        })
    }
}

struct DeploymentClient {
    pub http_client: reqwest::Client,
    pub subgraph_client: Mutex<GraphCoreSubgraphClient>,
    pub status: Option<Eventual<DeploymentStatus>>,
    pub query_url: Url,
}

impl DeploymentClient {
    pub fn new(http_client: reqwest::Client, details: DeploymentDetails) -> Self {
        let subgraph_client = Mutex::new(
            GraphCoreSubgraphClient::builder(http_client.clone(), details.query_url.clone())
                .with_auth_token(details.query_auth_token)
                .build(),
        );
        Self {
            http_client,
            subgraph_client,
            status: details
                .deployment
                .zip(details.status_url)
                .map(|(deployment, url)| monitor_deployment_status(deployment, url)),
            query_url: details.query_url,
        }
    }

    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        query: impl IntoRequestParameters + Send,
    ) -> Result<Result<T, String>, anyhow::Error> {
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
            .subgraph_client
            .lock()
            .await
            .query::<T>(query)
            .await
            .inspect_err(|err| {
                warn!(
                    "Failed to query subgraph deployment `{}`: {}",
                    self.query_url, err
                );
            }))
    }

    pub async fn paginated_query<T: for<'de> Deserialize<'de>>(
        &self,
        query: String,
        items_per_page: usize,
    ) -> Result<Vec<T>, anyhow::Error> {
        if let Some(ref status) = self.status {
            let deployment_status = status.value().await.expect("reading deployment status");

            if !deployment_status.synced || &deployment_status.health != "healthy" {
                return Err(anyhow!(
                    "Deployment `{}` is not ready or healthy to be queried",
                    self.query_url
                ));
            }
        }
        self.subgraph_client
            .lock()
            .await
            .paginated_query::<T>(query, items_per_page)
            .await
            .map_err(|err| {
                warn!(
                    "Failed to query subgraph deployment `{}`: {}",
                    self.query_url, err
                );
                anyhow!(err)
            })
    }

    pub async fn query_raw(&self, body: Bytes) -> Result<reqwest::Response, anyhow::Error> {
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
            .header(header::USER_AGENT, "indexer-common")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
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
        query: impl IntoRequestParameters + Send + Clone,
    ) -> Result<Result<T, String>, anyhow::Error> {
        // Try the local client first; if that fails, log the error and move on
        // to the remote client
        if let Some(ref local_client) = self.local_client {
            match local_client.query(query.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) => warn!(
                    "Failed to query local subgraph deployment `{}`, trying remote deployment next: {}",
                    local_client.query_url, err
                ),
            }
        }

        // Try the remote client
        self.remote_client.query::<T>(query).await.map_err(|err| {
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

    pub async fn paginated_query<T: for<'de> Deserialize<'de>>(
        &self,
        query: String,
        items_per_page: usize,
    ) -> Result<Vec<T>, anyhow::Error> {
        // Try the local client first; if that fails, log the error and move on
        // to the remote client
        if let Some(ref local_client) = self.local_client {
            match local_client.paginated_query::<T>(query.clone(), items_per_page).await {
                Ok(response) => return Ok(response),
                Err(err) => warn!(
                    "Failed to query local subgraph deployment `{}`, trying remote deployment next: {}",
                    local_client.query_url, err
                ),
            }
        }
        // Try the remote client
        self.remote_client
            .paginated_query::<T>(query, 1000)
            .await
            .map_err(|err| {
                warn!(
                    "Failed to query remote subgraph deployment `{}`: {}",
                    self.remote_client.query_url, err
                );
                anyhow!(err)
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
            .query::<Value>(Query::new(
                r#"
                query {
                 	graphNetwork(id: 1) {
                 		currentEpoch
                 	}
                }
                "#,
            ))
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
        let data = client
            .query::<Value>(Query::new("{ user(id: 1} { name } }"))
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data, json!({ "user": { "name": "local" } }));
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
        let data = client
            .query::<Value>(Query::new("{ user(id: 1} { name } }"))
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data, json!({ "user": { "name": "remote" } }));
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
        let data = client
            .query::<Value>(Query::new("{ user(id: 1} { name } }"))
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data, json!({ "user": { "name": "remote" } }));
    }
}
