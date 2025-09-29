// Copyright 2025-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{env, process::Command};

use anyhow::{anyhow, Context, Result};
use regex::Regex;

/// Minimal wrapper around the `indexer-cli` Docker service used in integration tests.
///
/// This struct is intentionally tiny; we only need the docker container name and
/// the target network (e.g., "hardhat").
/// this abstraction is planned to be used in our integration tests
/// to close allocations programmatically along with getting allocations statuses and information
/// this is still a WIP, we found some issues with the indexer-cli container, that needs more
/// investigation(the indexer-cli package itsefl seems to have a bug)
pub struct IndexerCli {
    container: String,
    network: String,
}

impl IndexerCli {
    /// Create a new wrapper.
    ///
    /// - `network` is the Graph network argument passed to `graph indexer ... --network {network}`
    /// - Container name defaults to "indexer-cli" and can be overridden by env `INDEXER_CLI_CONTAINER`.
    pub fn new<S: Into<String>>(network: S) -> Self {
        // Prefer explicit override via INDEXER_CLI_CONTAINER, fall back to CONTAINER_NAME
        // (as mentioned in contrib/indexer-cli/README.md), then to the default "indexer-cli".
        let container = env::var("CONTAINER_NAME").unwrap_or_else(|_| "indexer-cli".to_string());
        Self {
            container,
            network: network.into(),
        }
    }

    /// List allocation IDs by invoking:
    ///   docker exec {container} graph indexer allocations get --network {network}
    ///
    /// Returns a list of 0x-prefixed addresses extracted from stdout in order of appearance (deduplicated).
    pub fn list_allocations(&self) -> Result<Vec<String>> {
        let stdout = self.exec([
            "graph",
            "indexer",
            "allocations",
            "get",
            "--network",
            &self.network,
        ])?;
        Ok(parse_eth_addresses(&stdout))
    }

    /// Close a specific allocation by invoking:
    ///   docker exec {container} graph indexer allocations close {allocation} <poi> --network {network} --force
    ///
    /// For integration testing convenience, we use a zero POI by default as in the README example.
    pub fn close_allocation(&self, allocation: &str) -> Result<()> {
        let re = Regex::new(r"^0x[a-fA-F0-9]{40}$").unwrap();
        if !re.is_match(allocation) {
            return Err(anyhow!("Invalid allocation ID: {allocation}"));
        }
        // Zero POI (32 bytes of zeros)
        const ZERO_POI: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
        let _stdout = self.exec([
            "graph",
            "indexer",
            "allocations",
            "close",
            allocation,
            ZERO_POI,
            "--network",
            &self.network,
            "--force",
        ])?;
        Ok(())
    }

    /// Helper to run `docker exec {container} ...` and capture stdout/stderr.
    fn exec<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = Command::new("docker");
        cmd.arg("exec").arg(&self.container);
        for a in args {
            cmd.arg(a.as_ref());
        }
        let output = cmd.output().with_context(|| {
            format!(
                "failed to spawn docker exec for container {}",
                self.container
            )
        })?;
        if !output.status.success() {
            return Err(anyhow!(
                "docker exec exited with {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Extract all 0x-addresses (40 hex chars) from an arbitrary output string, preserving order and de-duplicating.
fn parse_eth_addresses(input: &str) -> Vec<String> {
    let re = Regex::new(r"0x[a-fA-F0-9]{40}").unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(input) {
        let addr = m.as_str().to_string();
        if seen.insert(addr.clone()) {
            out.push(addr);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_eth_addresses;

    #[test]
    fn parse_addresses_mixed_content() {
        let s = "some text 0xAbCdEf0123456789aBCdEf0123456789abCDef01 and again 0xabcdef0123456789abcdef0123456789abcdef01 and dup 0xabcdef0123456789abcdef0123456789abcdef01";
        let addrs = parse_eth_addresses(s);
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0], "0xAbCdEf0123456789aBCdEf0123456789abCDef01");
        assert_eq!(addrs[1], "0xabcdef0123456789abcdef0123456789abcdef01");
    }

    #[test]
    fn parse_addresses_none() {
        let s = "no addresses here";
        let addrs = parse_eth_addresses(s);
        assert!(addrs.is_empty());
    }

    #[test]
    fn parse_addresses_case_insensitive() {
        let s =
            "0xABCDEF0123456789ABCDEF0123456789ABCDEF01 0xabcdef0123456789abcdef0123456789abcdef02";
        let addrs = parse_eth_addresses(s);
        assert_eq!(addrs.len(), 2);
    }
}
