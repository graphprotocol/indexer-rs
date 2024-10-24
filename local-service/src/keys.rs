// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use std::{io, path::PathBuf};

use alloy::primitives::Address;
use indexer_common::prelude::{Allocation, AttestationSigner};
use toml::Value;

#[derive(Clone, Debug)]
pub struct Signer {
    pub signer: AttestationSigner,
    pub allocation: Allocation,
}

impl Signer {
    pub fn new(signer: AttestationSigner, allocation: Allocation) -> Self {
        Signer { signer, allocation }
    }
}

pub fn get_mnemonic_from_toml(index: &str, key: &str) -> io::Result<String> {
    Ok(get_from_toml(index, key)?.to_string())
}

pub fn get_indexer_address_from_toml(index: &str, key: &str) -> io::Result<Address> {
    Ok(get_from_toml(index, key)?.parse().unwrap())
}

fn get_from_toml(index: &str, key: &str) -> io::Result<String> {
    let toml = get_value_from_toml().unwrap();

    if let Some(item) = toml
        .get(index)
        .and_then(|index| index.get(key))
        .and_then(|value| value.as_str())
    {
        Ok(item.to_string())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "Config item not found in TOML file",
        ))
    }
}

fn get_value_from_toml() -> io::Result<Value> {
    let config_path = get_config_path();
    let content = std::fs::read_to_string(config_path)?;
    let toml: Value = content.parse().unwrap();
    Ok(toml)
}

fn get_config_path() -> PathBuf {
    // CARGO_MANIFEST_DIR points to the root of the current crate
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("config.toml"); // Add the file name
    path
}
