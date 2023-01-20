use anyhow;
use bs58;
use ethers_core::abi::AbiEncode;
use log::{debug, error, info, trace, warn, Log};
use regex::Regex;
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Subgraph identifier type: Subgraph name with field 'value'
pub struct SubgraphName {
    value: String,
}

/// Implement SubgraphName constructor
impl SubgraphName {
    fn new(name: &str) -> SubgraphName {
        SubgraphName {
            // or call this field name
            value: name.to_owned(),
        }
    }
}

/// Implement SubgraphName String representation
impl ToString for SubgraphName {
    fn to_string(&self) -> String {
        format!("{}", self.value)
    }
}

/// Security: Input validation
pub fn bytes32Check() -> Regex {
    Regex::new(r"^0x[0-9a-f]{64}$").unwrap()
}

/// Security: Input Validation
pub fn multiHashCheck() -> Regex {
    Regex::new(r"^Qm[1-9a-km-zA-HJ-NP-Z]{44}$").unwrap()
}

/// Subgraph identifier type: SubgraphDeploymentID with field 'value'
#[derive(Debug)]
pub struct SubgraphDeploymentID {
    // Hexadecimal (bytes32) representation of the subgraph deployment Id
    value: String,
}

/// Implement SubgraphDeploymentID functions
impl SubgraphDeploymentID {
    /// Assume byte 32
    /// Later add Security: Input validation
    pub fn new(id: String) -> SubgraphDeploymentID {
        SubgraphDeploymentID {
            value: id.to_owned(),
        }
    }

    fn bytes32(&self) -> String {
        return self.value.clone();
    }

    fn ipfsHash(&self) -> String {
        let value = self.value.clone();
        let mut bytes: Vec<u8> = vec![0x12, 0x20];
        bytes.extend(value.as_bytes().to_vec());
        let encoded = bytes.encode();
        String::from_utf8(encoded).unwrap()
    }
}

impl ToString for SubgraphDeploymentID {
    fn to_string(&self) -> String {
        format!("{}", self.value)
    }
}
pub struct Signature {
    v: i64,
    r: String,
    s: String,
}

pub struct QueryResult {
    graphQLResponse: String,
    attestation: Option<Signature>,
}

#[derive(Debug, Clone)]
pub struct UnattestedQueryResult {
    pub graphQLResponse: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response<T> {
    pub result: T,
    pub status: i64,
}

/// Free query do not need signature, receipt, signers
/// Also ignore metrics for now
/// Later add along with PaidQuery
#[derive(Debug)]
pub struct FreeQuery {
    pub subgraph_deployment_id: SubgraphDeploymentID,
    pub query: String,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error("The subgraph is in a failed state")]
    IndexingError,
    #[error("Bad or invalid entity data found in the subgraph: {}", .0.to_string())]
    BadData(anyhow::Error),
    #[error("Unknown error: {0}")]
    Other(anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct QueryProcessor {
    client: Client,
    base: Url,
    graph_node: GraphNodeInstance,
    network_subgraph: Url,
}

#[derive(Debug, Clone)]
struct GraphNodeInstance {
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

    // async fn make_get_request(&self, endpoint: &str) -> Result<reqwest::Response, reqwest::Error> {
    //     self.client
    //         .get(&format!("{}/{}", self.base_url, endpoint))
    //         .send()
    //         .await
    // }

    async fn subgraph_query(
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

    async fn network_query(
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

impl QueryProcessor {
    pub fn new(graph_node_endpoint: &str, network_subgraph_endpoint: &str) -> QueryProcessor {
        let graph_node = GraphNodeInstance::new(graph_node_endpoint.clone());

        QueryProcessor {
            client: Client::new(),
            base: Url::parse(graph_node_endpoint).expect("Could not parse graph node endpoint"),
            graph_node,
            network_subgraph: Url::parse(network_subgraph_endpoint).expect("Could not parse graph node endpoint"),
        }
    }

    pub async fn execute_free_query(
        &self,
        query: FreeQuery,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self
            .graph_node
            .subgraph_query(&query.subgraph_deployment_id.value, query.query)
            .await?;

        Ok(Response {
            result: response,
            status: 200,
        })
    }

    pub async fn execute_network_free_query(
        &self,
        query: String,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self
            .graph_node
            .network_query(self.network_subgraph.clone(), query)
            .await?;

        Ok(Response {
            result: response,
            status: 200,
        })
    }
}
