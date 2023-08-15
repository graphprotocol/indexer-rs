// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::anyhow;
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::query_processor::{QueryError, UnattestedQueryResult};

/// Graph node query wrapper.
///
/// This is Arc internally, so it can be cloned and shared between threads.
#[derive(Debug, Clone)]
pub struct GraphNodeInstance {
    client: Client, // it is Arc
    subgraphs_base_url: Arc<Url>,
    network_subgraph_url: Arc<Url>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphQLQuery {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
}

impl GraphNodeInstance {
    pub fn new(endpoint: &str, network_subgraph_id: &str) -> GraphNodeInstance {
        let subgraphs_base_url = Url::parse(endpoint)
            .and_then(|u| u.join("/subgraphs/id/"))
            .expect("Could not parse graph node endpoint");
        let network_subgraph_url = subgraphs_base_url
            .join(network_subgraph_id)
            .expect("Could not parse graph node endpoint");
        let client = reqwest::Client::builder()
            .user_agent("indexer-service")
            .build()
            .expect("Could not build a client to graph node query endpoint");
        GraphNodeInstance {
            client,
            subgraphs_base_url: Arc::new(subgraphs_base_url),
            network_subgraph_url: Arc::new(network_subgraph_url),
        }
    }

    pub async fn subgraph_query_raw(
        &self,
        subgraph_id: &str,
        data: String,
    ) -> Result<UnattestedQueryResult, QueryError> {
        let request = self
            .client
            .post(self.subgraphs_base_url.join(subgraph_id).map_err(|e| {
                QueryError::Other(anyhow!(
                    "Could not build subgraph query URL: {}",
                    e.to_string()
                ))
            })?)
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
}
