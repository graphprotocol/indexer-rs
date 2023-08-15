// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::query_processor::UnattestedQueryResult;

#[derive(Debug, Clone)]
pub struct GraphNodeInstance {
    client: Client,
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphQLQuery {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
}

impl GraphNodeInstance {
    pub fn new(base_url: &str) -> GraphNodeInstance {
        let client = reqwest::Client::builder()
            .user_agent("indexer-service")
            .build()
            .expect("Could not build a client to graph node query endpoint");
        GraphNodeInstance {
            client,
            base_url: base_url.to_string(),
        }
    }

    pub async fn subgraph_query_raw(
        &self,
        endpoint: &str,
        body: String,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let request = self
            .client
            .post(format!("{}/subgraphs/id/{}", self.base_url, endpoint))
            .body(body)
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
        endpoint: Url,
        body: String,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let request = self
            .client
            .post(endpoint)
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
        endpoint: Url,
        query: String,
        variables: Option<Value>,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let body = GraphQLQuery { query, variables };

        self.network_query_raw(
            endpoint,
            serde_json::to_string(&body).expect("serialize network GraphQL query"),
        )
        .await
    }
}
