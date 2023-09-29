// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use reqwest::{header, Client, Url};
use serde::de::Deserialize;
use serde_json::Value;
use toolshed::graphql::http::Response;

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

    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        body: &Value,
    ) -> Result<Response<T>, reqwest::Error> {
        self.client
            .post(Url::clone(&self.network_subgraph_url))
            .json(body)
            .header(header::CONTENT_TYPE, "application/json")
            .send()
            .await
            .and_then(|response| response.error_for_status())?
            .json::<Response<T>>()
            .await
    }
}

#[cfg(test)]
mod test {
    use serde_json::json;
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
    async fn test_network_query() {
        let _mock_server = mock_graph_node_server().await;

        // Check that the response is valid JSON
        let result = network_subgraph()
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
}
