// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use thegraph::types::Address;

/// Load a hashmap of sender addresses and their corresponding aggregator endpoints
/// from a yaml file. We're using serde_yaml.
pub fn load_aggregator_endpoints(file_path: PathBuf) -> HashMap<Address, String> {
    let file = File::open(file_path).unwrap();
    let reader = BufReader::new(file);
    let endpoints: HashMap<Address, String> = serde_yaml::from_reader(reader).unwrap();
    endpoints
}

#[cfg(test)]
mod tests {
    use std::{io::Write, str::FromStr};

    use super::*;

    /// Test that we can load the aggregator endpoints from a yaml file.
    /// The test is going to create a temporary yaml file using tempfile, load it, and
    /// check that the endpoints are loaded correctly.
    #[test]
    fn test_load_aggregator_endpoints() {
        let named_temp_file = tempfile::NamedTempFile::new().unwrap();
        let mut temp_file = named_temp_file.reopen().unwrap();
        let yaml = r#"
                0xdeadbeefcafebabedeadbeefcafebabedeadbeef: https://example.com/aggregate-receipts
                0x0123456789abcdef0123456789abcdef01234567: https://other.example.com/aggregate-receipts
            "#;
        temp_file.write_all(yaml.as_bytes()).unwrap();
        let endpoints = load_aggregator_endpoints(named_temp_file.path().to_path_buf());
        assert_eq!(
            endpoints
                .get(&Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap()),
            Some(&"https://example.com/aggregate-receipts".to_string())
        );
    }
}
