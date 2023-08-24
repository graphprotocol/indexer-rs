// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// GraphQLQuery request to a reqwest client
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphQLQuery {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
}

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubgraphDeploymentID {
    // bytes32 subgraph deployment Id
    value: [u8; 32],
}

/// Implement deserialization for SubgraphDeploymentID
/// Deserialize from hex string or IPFS multihash string
impl<'de> Deserialize<'de> for SubgraphDeploymentID {
    fn deserialize<D>(deserializer: D) -> Result<SubgraphDeploymentID, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        SubgraphDeploymentID::new(&s).map_err(serde::de::Error::custom)
    }
}

/// Implement SubgraphDeploymentID functions
impl SubgraphDeploymentID {
    /// Construct SubgraphDeploymentID from a string
    /// Validate IPFS hash or hex format before decoding
    pub fn new(id: &str) -> anyhow::Result<SubgraphDeploymentID> {
        SubgraphDeploymentID::from_hex(id)
            .or_else(|_| SubgraphDeploymentID::from_ipfs_hash(id))
            .map_err(|_| anyhow::anyhow!("Invalid subgraph deployment ID: {}", id))
    }

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
    fn subgraph_deployment_id_input_validation_success() {
        let deployment_id = "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884abf";
        let ipfs_hash = "QmcPHxcC2ZN7m79XfYZ77YmF4t9UCErv87a9NFKrSLWKtJ";

        assert_eq!(
            SubgraphDeploymentID::new(ipfs_hash).unwrap().to_string(),
            deployment_id
        );

        assert_eq!(
            SubgraphDeploymentID::new(deployment_id)
                .unwrap()
                .ipfs_hash(),
            ipfs_hash
        );
    }

    #[test]
    fn subgraph_deployment_id_input_validation_fail() {
        let invalid_deployment_id =
            "0xd0b0e5b65df45a3fff1a653b4188881318e8459d3338f936aab16c4003884a";
        let invalid_ipfs_hash = "Qm1234";

        let res = SubgraphDeploymentID::new(invalid_deployment_id);
        assert!(res.is_err());

        let res = SubgraphDeploymentID::new(invalid_ipfs_hash);
        assert!(res.is_err());
    }
}
