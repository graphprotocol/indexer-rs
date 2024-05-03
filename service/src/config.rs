// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use figment::{
    providers::{Format, Toml},
    Figment,
};
use indexer_common::indexer_service::http::IndexerServiceConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub common: IndexerServiceConfig,
}

impl Config {
    pub fn load(filename: &PathBuf) -> Result<Self, figment::Error> {
        Figment::new().merge(Toml::file(filename)).extract()
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use super::*;

    /// Test loading the minimal configuration example file.
    /// Makes sure that the minimal template is up to date with the code.
    #[test]
    fn test_minimal_config() {
        Config::load(&PathBuf::from("service/minimal-config-example.toml")).unwrap();
    }

    /// Test that the maximal configuration file is up to date with the code.
    /// Make sure that `test_minimal_config` passes before looking at this.
    #[test]
    fn test_maximal_config() {
        // Generate full config by deserializing the minimal config and let the code fill in the defaults.
        let max_config =
            Config::load(&PathBuf::from("service/minimal-config-example.toml")).unwrap();
        // Deserialize the full config example file
        let max_config_file: toml::Value = toml::from_str(
            fs::read_to_string("maximal-config-example.toml")
                .unwrap()
                .as_str(),
        )
        .unwrap();

        assert_eq!(toml::Value::try_from(max_config).unwrap(), max_config_file);
    }
}
