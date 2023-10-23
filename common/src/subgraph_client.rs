// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::anyhow;
use graphql::http::Response;
use reqwest::{header, Client, Url};
use serde::de::Deserialize;
use serde_json::Value;

/// Network subgraph query wrapper
///
/// This is Arc internally, so it can be cloned and shared between threads.
#[derive(Debug, Clone)]
pub struct SubgraphClient {
    client: Client, // it is Arc
    subgraph_url: Arc<Url>,
}

impl SubgraphClient {
    pub fn new(name: &str, query_url: &str) -> Result<Self, anyhow::Error> {
        let query_url = Url::parse(query_url).map_err(|e| {
            anyhow!(
                "Could not parse `{}` subgraph query URL `{}`: {}",
                name,
                query_url,
                e
            )
        })?;

        let client = reqwest::Client::builder()
            .user_agent("indexer-common")
            .build()
            .map_err(|err| {
                anyhow!(
                    "Could not build a client for `{name}` subgraph query URL `{query_url}`: {err}"
                )
            })
            .expect("Building subgraph client");

        Ok(Self {
            client,
            subgraph_url: Arc::new(query_url),
        })
    }

    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        body: &Value,
    ) -> Result<Response<T>, reqwest::Error> {
        self.client
            .post(Url::clone(&self.subgraph_url))
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

    use crate::test_vectors;

    use super::*;

    const GRAPH_NODE_STATUS_ENDPOINT: &str = "http://localhost:8000/";
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
        SubgraphClient::new("network-subgraph", NETWORK_SUBGRAPH_URL).unwrap()
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
}
