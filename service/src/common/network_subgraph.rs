// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::types::GraphQLQuery;
use crate::query_processor::{QueryError, UnattestedQueryResult};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response<T> {
    pub result: T,
    pub status: i64,
}

/// Network subgraph query wrapper
///
/// This is Arc internally, so it can be cloned and shared between threads.
#[derive(Debug, Clone)]
pub struct NetworkSubgraph {
    client: Client, // it is Arc
    network_subgraph_url: Arc<Url>,
}

impl NetworkSubgraph {
    pub fn new(
        graph_node_query_endpoint: Option<&str>,
        deployment: Option<&str>,
        network_subgraph_url: &str,
    ) -> NetworkSubgraph {
        //TODO: Check indexing status of the local network subgraph deployment
        //if the deployment is healthy and synced, use local_network_subgraph_endpoint
        let _local_network_subgraph_endpoint = match (graph_node_query_endpoint, deployment) {
            (Some(endpoint), Some(id)) => {
                Some(NetworkSubgraph::local_deployment_endpoint(endpoint, id))
            }
            _ => None,
        };

        let network_subgraph_url =
            Url::parse(network_subgraph_url).expect("Could not parse network subgraph url");

        let client = reqwest::Client::builder()
            .user_agent("indexer-service")
            .build()
            .expect("Could not build a client to graph node query endpoint");

        NetworkSubgraph {
            client,
            network_subgraph_url: Arc::new(network_subgraph_url),
        }
    }

    pub fn local_deployment_endpoint(graph_node_query_endpoint: &str, deployment: &str) -> Url {
        Url::parse(graph_node_query_endpoint)
            .and_then(|u| u.join("/subgraphs/id/"))
            .and_then(|u| u.join(deployment))
            .expect("Could not parse graph node query endpoint for the network subgraph deployment")
    }

    pub async fn network_query_raw(
        &self,
        body: String,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let request = self
            .client
            .post(Url::clone(&self.network_subgraph_url))
            .body(body.clone())
            .header(header::CONTENT_TYPE, "application/json");

        let response = request.send().await?;

        // actually parse the JSON for the graphQL schema
        let response_text = response.text().await?;
        Ok(UnattestedQueryResult {
            graphql_response: response_text,
            attestable: false,
        })
    }

    pub async fn network_query(
        &self,
        query: String,
        variables: Option<Value>,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let body = GraphQLQuery { query, variables };

        self.network_query_raw(
            serde_json::to_string(&body).expect("serialize network GraphQL query"),
        )
        .await
    }

    pub async fn execute_network_free_query(
        &self,
        query: String,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self.network_query_raw(query).await?;

        Ok(Response {
            result: response,
            status: 200,
        })
    }
}

#[cfg(test)]
mod test {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const GRAPH_NODE_STATUS_ENDPOINT: &str = "http://localhost:8000/";
    const NETWORK_SUBGRAPH_ID: &str = "QmV614UpBCpuusv5MsismmPYu4KqLtdeNMKpiNrX56kw6u";
    const NETWORK_SUBGRAPH_URL: &str =
        "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli";

    async fn mock_graph_node_server() -> MockServer {
        let mock_server = MockServer::start().await;
        let mock = Mock::given(method("POST"))
            .and(path("/subgraphs/id/".to_string() + NETWORK_SUBGRAPH_ID))
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

    fn network_subgraph() -> NetworkSubgraph {
        NetworkSubgraph::new(
            Some(GRAPH_NODE_STATUS_ENDPOINT),
            Some(NETWORK_SUBGRAPH_ID),
            NETWORK_SUBGRAPH_URL,
        )
    }

    #[tokio::test]
    #[ignore] // Run only if explicitly specified
    async fn test_network_query() {
        let network_subgraph = network_subgraph();

        let query = r#""{\"data\":{\"graphNetwork\":{\"currentEpoch\":960}}}""#;

        let response = network_subgraph
            .network_query(query.to_string(), None)
            .await
            .unwrap();

        // Check that the response is valid JSON
        let _json: serde_json::Value = serde_json::from_str(&response.graphql_response).unwrap();
    }

    #[tokio::test]
    async fn test_network_query_mock() {
        let _mock_server = mock_graph_node_server().await;

        let network_subgraph = network_subgraph();

        let query = r#"
            query {
             	graphNetwork(id: 1) {
             		currentEpoch
             	}
            }
            "#;

        let response = network_subgraph
            .network_query(query.to_string(), None)
            .await
            .unwrap();

        // Check that the response is valid JSON
        let _json: serde_json::Value = serde_json::from_str(&response.graphql_response).unwrap();
    }
}
