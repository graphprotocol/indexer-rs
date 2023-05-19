use std::env;
use lazy_static::lazy_static;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use toml::Value;

use crate::common::indexer_error::{indexer_error, IndexerError};

lazy_static! {
    pub static ref DATABASE_URL: String = env::var("DATABASE_URL")
        .expect("DATABASE_URL is not set")
        .to_string();
}

/// Struct for version control
#[derive(Serialize, Debug, Clone)]
pub struct PackageVersion {
    version: String,
    dependencies: HashMap<String, String>,
}

/// Read the manfiest
fn read_manifest() -> Result<Value, IndexerError> {
    let toml_string = fs::read_to_string("Cargo.toml").map_err(|e| indexer_error(crate::common::indexer_error::IndexerErrorCode::IE074))?;
    let toml_value: Value = toml::from_str(&toml_string).map_err(|e| indexer_error(crate::common::indexer_error::IndexerErrorCode::IE074))?;
    Ok(toml_value)
}

/// Parse package versioning from the manifest
pub fn package_version() -> Result<PackageVersion, IndexerError> {
    read_manifest().and_then(|toml_file| {
        let pkg = toml_file.as_table().unwrap();
        let version = pkg.get("package").and_then(|p| p.get("version")).unwrap().as_str().unwrap().to_string();
        let dependencies = pkg.get("dependencies").and_then(|d| d.as_table()).unwrap();
        let indexer_native = dependencies.get("indexer-native").map(|d| d.as_str().unwrap().to_string());

        let release = PackageVersion {
            version,
            dependencies: match indexer_native {
                Some(indexer_native_version) => {
                    let mut map = HashMap::new();
                    map.insert("indexer-native".to_string(), indexer_native_version);
                    map
                }
                None => HashMap::new(),
            },
        };
        println!("{:?}", release);

        Ok(release)
    })
}

