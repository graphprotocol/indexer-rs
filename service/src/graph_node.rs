use reqwest::{header, Client, Url};

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
            .post(format!("{}/subgraphs/id/{}", self.base_url, endpoint))
            .body(data.clone())
            .header(header::CONTENT_TYPE, "application/json");

        let response = request.send().await?.text().await?;
        Ok(UnattestedQueryResult {
            graphql_response: response,
            attestable: true,
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
            graphql_response: response_text,
            attestable: false,
        })
    }
}
