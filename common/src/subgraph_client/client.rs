// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use axum::body::Bytes;
use eventuals::Eventual;
use graphql_client::GraphQLQuery;
use reqwest::{header, Url};
use serde_json::{Map, Value};
use thegraph::types::DeploymentId;
use thegraph_graphql_http::{
    graphql::{Document, IntoDocument},
    http::request::{IntoRequestParameters, RequestParameters},
};
use tracing::warn;

use super::monitor::{monitor_deployment_status, DeploymentStatus};

#[derive(Clone)]
pub struct Query {
    pub query: Document,
    pub variables: Map<String, Value>,
}

pub type ResponseResult<T> = Result<T, anyhow::Error>;

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

    pub async fn query<T: GraphQLQuery>(
        &self,
        variables: T::Variables,
    ) -> Result<ResponseResult<T::ResponseData>, anyhow::Error> {
        if let Some(ref status) = self.status {
            let deployment_status = status.value().await.expect("reading deployment status");

            if !deployment_status.synced || &deployment_status.health != "healthy" {
                return Err(anyhow!(
                    "Deployment `{}` is not ready or healthy to be queried",
                    self.query_url
                ));
            }
        }

        let body = T::build_query(variables);
        let reqwest_response = self
            .http_client
            .post(self.query_url.as_ref())
            .header(header::USER_AGENT, "indexer-common")
            .json(&body)
            .send()
            .await?;
        let response: graphql_client::Response<T::ResponseData> = reqwest_response.json().await?;

        // TODO handle partial responses
        Ok(match (response.data, response.errors) {
            (Some(data), None) => Ok(data),
            (_, Some(errors)) => Err(anyhow!("Query error")),
            (_, _) => Err(anyhow!("Invalid error")),
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

    use alloy_primitives::U256;
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

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../graphql/network.schema.graphql",
        query_path = "../graphql/epoch.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    struct CurrentEpoch;

    #[tokio::test]
    async fn test_network_query() {
        let _mock_server = mock_graph_node_server().await;

        // Check that the response is valid JSON
        let result = network_subgraph_client()
            .query::<CurrentEpoch, _>(current_epoch::Variables {})
            .await
            .unwrap();

        assert!(result.is_ok());
    }

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../graphql/test.schema.graphql",
        query_path = "../graphql/user.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    struct UserQuery;

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
            .query::<UserQuery, _>(user_query::Variables {})
            .await
            .expect("Query should succeed")
            .expect("Query result should have a value");

        assert_eq!(data.user.name, "remote".to_string());
    }
}
