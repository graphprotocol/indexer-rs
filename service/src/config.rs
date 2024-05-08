// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use anyhow::Result;
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
    pub fn load(filename: &PathBuf) -> Result<Self> {
        let config_str = std::fs::read_to_string(filename)?;
        let config_str = shellexpand::env(&config_str)?;
        Figment::new()
            .merge(Toml::string(&config_str))
            .extract()
            .map_err(|e| e.into())
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use super::*;

    /// Test loading the minimal configuration example file.
    /// Makes sure that the minimal template is up to date with the code.
    /// Note that it doesn't check that the config is actually minimal, but rather that all missing
    /// fields have defaults. The burden of making sure the config is minimal is on the developer.
    #[test]
    fn test_minimal_config() {
        Config::load(&PathBuf::from("minimal-config-example.toml")).unwrap();
    }

    /// Test that the maximal configuration file is up to date with the code.
    /// Make sure that `test_minimal_config` passes before looking at this.
    #[test]
    fn test_maximal_config() {
        // Generate full config by deserializing the minimal config and let the code fill in the defaults.
        let max_config = Config::load(&PathBuf::from("minimal-config-example.toml")).unwrap();
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
