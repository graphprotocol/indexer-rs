use log::error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::graph_node::GraphNodeInstance;

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
        self.value.to_string()
    }
}

/// Subgraph identifier type: SubgraphDeploymentID with field 'value'
#[derive(Debug)]
pub struct SubgraphDeploymentID {
    // bytes32 subgraph deployment Id
    value: [u8; 32],
}

/// Implement SubgraphDeploymentID functions
impl SubgraphDeploymentID {
    /// Construct SubgraphDeploymentID from a 32 bytes hex string.
    /// The '0x' prefix is optional.
    ///
    /// Returns an error if the input is not a valid hex string or if the input is not 32 bytes long.
    pub fn from_hex(id: &str) -> anyhow::Result<SubgraphDeploymentID> {
        let mut buf = [0u8; 32];
        hex::decode_to_slice(id.trim_start_matches("0x"), &mut buf)?;
        Ok(SubgraphDeploymentID { value: buf })
    }

    /// Construct SubgraphDeploymentID from a 34 bytes IPFS multihash string.
    /// The 'Qm' prefix is mandatory.
    ///
    /// Returns an error if the input is not a valid IPFS multihash string or if the input is not 34 bytes long.
    pub fn from_ipfs_hash(hash: &str) -> anyhow::Result<SubgraphDeploymentID> {
        let bytes = bs58::decode(hash).into_vec()?;
        let value = bytes[2..].try_into()?;
        Ok(SubgraphDeploymentID { value })
    }

    /// Returns the subgraph deployment ID as a 32 bytes array.
    pub fn bytes32(&self) -> [u8; 32] {
        self.value
    }

    /// Returns the subgraph deployment ID as a 34 bytes IPFS multihash string.
    pub fn ipfs_hash(&self) -> String {
        let value = self.value;
        let mut bytes: Vec<u8> = vec![0x12, 0x20];
        bytes.extend(value.to_vec());
        bs58::encode(bytes).into_string()
    }

    /// Returns the subgraph deployment ID as a 32 bytes hex string.
    /// The '0x' prefix is included.
    pub fn hex(&self) -> String {
        format!("0x{}", hex::encode(self.value))
    }
}

impl ToString for SubgraphDeploymentID {
    fn to_string(&self) -> String {
        self.hex()
    }
}
pub struct Signature {
    v: i64,
    r: String,
    s: String,
}

pub struct QueryResult {
    graphql_response: String,
    attestation: Option<Signature>,
}

#[derive(Debug, Clone)]
pub struct UnattestedQueryResult {
    pub graphql_response: String,
    pub attestable: bool,
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

impl QueryProcessor {
    pub fn new(graph_node_endpoint: &str, network_subgraph_endpoint: &str) -> QueryProcessor {
        let graph_node = GraphNodeInstance::new(graph_node_endpoint);

        QueryProcessor {
            client: Client::new(),
            base: Url::parse(graph_node_endpoint).expect("Could not parse graph node endpoint"),
            graph_node,
            network_subgraph: Url::parse(network_subgraph_endpoint)
                .expect("Could not parse graph node endpoint"),
        }
    }

    pub async fn execute_free_query(
        &self,
        query: FreeQuery,
    ) -> Result<Response<UnattestedQueryResult>, QueryError> {
        let response = self
            .graph_node
            .subgraph_query(&query.subgraph_deployment_id.ipfs_hash(), query.query)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_ipfs_multihash() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let expected_ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::from_hex(deployment_id)
                .unwrap()
                .ipfs_hash(),
            expected_ipfs_hash
        );
    }

    #[test]
    fn ipfs_multihash_to_hex() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::from_ipfs_hash(ipfs_hash)
                .unwrap()
                .to_string(),
            deployment_id
        );
    }

    #[test]
    fn subgraph_deployment_id_input_validation() {
        let invalid_deployment_id =
            "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884a";
        let invalid_ipfs_hash = "Qm1234";

        let res = SubgraphDeploymentID::from_hex(invalid_deployment_id);
        assert!(res.is_err());

        let res = SubgraphDeploymentID::from_ipfs_hash(invalid_ipfs_hash);
        assert!(res.is_err());
    }
}
