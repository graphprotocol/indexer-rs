// Copyright 2025-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    sync::Once,
};

// Ensure we only load and apply the .env once per process
static LOAD_ONCE: Once = Once::new();

/// Load environment variables from `integration-tests/.env` (symlinked to contrib/local-network/.env).
///
/// - Does nothing if the file is missing.
/// - Skips comments and blank lines.
/// - Supports optional `export KEY=VALUE` prefix.
/// - Keeps existing process env values (won't override existing keys).
pub fn load_integration_env() {
    LOAD_ONCE.call_once(|| {
        let path = Path::new(".env");
        if !path.exists() {
            return;
        }
        if let Ok(file) = File::open(path) {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if let Some((key, val)) = parse_env_line(&line) {
                    if env::var(&key).is_err() {
                        env::set_var(key, val);
                    }
                }
            }
        }
    });
}

fn parse_env_line(line: &str) -> Option<(String, String)> {
    let mut s = line.trim();
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    if let Some(rest) = s.strip_prefix("export ") {
        s = rest.trim();
    }

    // Split on the first '=' only
    let (k, v) = s.split_once('=')?;
    let key = k.trim();
    if key.is_empty() || key.starts_with('#') {
        return None;
    }

    // Trim value, strip surrounding quotes, and remove trailing inline comments
    let mut val = v.trim().to_string();
    // Remove surrounding quotes if present
    if (val.starts_with('"') && val.ends_with('"'))
        || (val.starts_with('\'') && val.ends_with('\''))
    {
        val = val[1..val.len().saturating_sub(1)].to_string();
    }
    // Remove trailing inline comments beginning with space+hash
    if let Some(idx) = val.find(" #") {
        val.truncate(idx);
        val = val.trim().to_string();
    }
    Some((key.to_string(), val))
}
