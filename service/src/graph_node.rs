use anyhow;
use bs58;
use ethers_core::abi::AbiEncode;
use log::{debug, error, info, trace, warn, Log};
use regex::Regex;
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::query_processor::UnattestedQueryResult;

#[derive(Debug, Clone)]
pub struct GraphNodeInstance {
    client: Client,
    base_url: String,
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

    pub async fn subgraph_query(
        &self,
        endpoint: &str,
        data: String,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let request = self
            .client
            .post(&format!("{}/subgraphs/id/{}", self.base_url, endpoint))
            .body(data.clone())
            .header(header::CONTENT_TYPE, "application/json");

        let response = request.send().await?.text().await?;
        Ok(UnattestedQueryResult {
            graphQLResponse: response,
        })
    }

    pub async fn network_query(
        &self,
        endpoint: Url,
        data: String,
    ) -> Result<UnattestedQueryResult, reqwest::Error> {
        let request = self
            .client
            .post(endpoint)
            .body(data.clone())
            .header(header::CONTENT_TYPE, "application/json");

        let response = request.send().await?;

        // actually parse the JSON for the graphQL schema
        let response_text = response.text().await?;
        Ok(UnattestedQueryResult {
            graphQLResponse: response_text,
        })
        // let response_json: serde_json::Value = response.json()?;

        // match response_body.errors.as_deref() {
        //     Some([]) | None => {
        //         METRICS.set_subgraph_indexing_errors(false);
        //     }
        //     Some(errors) => {
        //         // We only deal with the first error and ignore the rest.
        //         let e = &errors[0];
        //         if e.message == "indexing_error" {
        //             METRICS.set_subgraph_indexing_errors(true);
        //             return Err(SubgraphQueryError::IndexingError);
        //         } else {
        //             return Err(SubgraphQueryError::Other(anyhow::anyhow!("{}", e.message)));
        //         }
        //     }
        // }

        // let data = if let Some(data) = response_body.data {
        //     data
        // } else {
        //     return Err(SubgraphQueryError::Other(anyhow::anyhow!(
        //         "No response data"
        //     )));
        // };

        // // Validate status
        // if !response.status().is_success() {
        //     let status = response.status();
        //     return QueryError::Transport;
        // }
    }
}