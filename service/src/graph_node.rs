// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use reqwest::{header, Client, Url};
use std::sync::Arc;
use toolshed::thegraph::DeploymentId;

use crate::query_processor::{QueryError, UnattestedQueryResult};

/// Graph node query wrapper.
///
/// This is Arc internally, so it can be cloned and shared between threads.
#[derive(Debug, Clone)]
pub struct GraphNodeInstance {
    client: Client, // it is Arc
    subgraphs_base_url: Arc<Url>,
}

impl GraphNodeInstance {
    pub fn new(endpoint: &str) -> GraphNodeInstance {
        let subgraphs_base_url = Url::parse(endpoint)
            .and_then(|u| u.join("/subgraphs/id/"))
            .expect("Could not parse graph node endpoint");
        let client = reqwest::Client::builder()
            .user_agent("indexer-service")
            .build()
            .expect("Could not build a client to graph node query endpoint");
        GraphNodeInstance {
            client,
            subgraphs_base_url: Arc::new(subgraphs_base_url),
        }
    }

    pub async fn subgraph_query_raw(
        &self,
        subgraph_id: &DeploymentId,
        data: String,
    ) -> Result<UnattestedQueryResult, QueryError> {
        let request = self
            .client
            .post(
                self.subgraphs_base_url
                    .join(&subgraph_id.to_string())
                    .map_err(|e| {
                        QueryError::Other(anyhow!(
                            "Could not build subgraph query URL: {}",
                            e.to_string()
                        ))
                    })?,
            )
            .body(data)
            .header(header::CONTENT_TYPE, "application/json");

        let response = request.send().await?;
        let attestable = response
            .headers()
            .get("graph-attestable")
            .map_or(false, |v| v == "true");

        Ok(UnattestedQueryResult {
            graphql_response: response.text().await?,
            attestable,
        })
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use lazy_static::lazy_static;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    lazy_static! {
        static ref NETWORK_SUBGRAPH_ID: DeploymentId =
            DeploymentId::from_str("QmV614UpBCpuusv5MsismmPYu4KqLtdeNMKpiNrX56kw6u").unwrap();
    }

    async fn mock_graph_node_server() -> MockServer {
        let mock_server = MockServer::start().await;
        let mock = Mock::given(method("POST"))
            .and(path(
                "/subgraphs/id/".to_string() + &NETWORK_SUBGRAPH_ID.to_string(),
            ))
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

    async fn local_graph_node() -> GraphNodeInstance {
        let graph_node_endpoint = std::env::var("GRAPH_NODE_ENDPOINT")
            .expect("GRAPH_NODE_ENDPOINT env variable is not set");

        GraphNodeInstance::new(&graph_node_endpoint)
    }

    /// Also tests against the network subgraph, but using the `subgraph_query_raw` method
    #[tokio::test]
    #[ignore] // Run only if explicitly specified
    async fn test_subgraph_query_local() {
        let network_subgraph_id = DeploymentId::from_str(
            &std::env::var("NETWORK_SUBGRAPH_ID")
                .expect("NETWORK_SUBGRAPH_ID env variable is not set"),
        )
        .unwrap();

        let graph_node = local_graph_node().await;

        let query = r#"
            query {
             	graphNetwork(id: 1) {
             		currentEpoch
             	}
            }
        "#;

        let query_json = json!({
            "query": query,
            "variables": {}
        });

        let response = graph_node
            .subgraph_query_raw(&network_subgraph_id, query_json.to_string())
            .await
            .unwrap();

        // Check that the response is valid JSON
        let _json: serde_json::Value = serde_json::from_str(&response.graphql_response).unwrap();
    }

    /// Also tests against the network subgraph, but using the `subgraph_query_raw` method
    #[tokio::test]
    async fn test_subgraph_query() {
        let mock_server = mock_graph_node_server().await;

        let graph_node = GraphNodeInstance::new(&mock_server.uri());

        let query = r#"
            query {
             	graphNetwork(id: 1) {
             		currentEpoch
             	}
            }
            "#;

        let query_json = json!({
            "query": query,
            "variables": {}
        });

        let response = graph_node
            .subgraph_query_raw(&NETWORK_SUBGRAPH_ID, query_json.to_string())
            .await
            .unwrap();

        // Check that the response is valid JSON
        let _json: serde_json::Value = serde_json::from_str(&response.graphql_response).unwrap();
    }
}
