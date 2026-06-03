// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::State, Json};
use serde::Serialize;
use std::collections::BTreeMap;

/// Indexer DIPs pricing signal. Derived from `DipsConfig`
/// at startup and served as-is on every request.
#[derive(Clone, Debug, Serialize)]
pub struct DipsInfo {
    pub min_grt_per_30_days: BTreeMap<String, String>,
    pub min_grt_per_billion_entities_per_30_days: String,
    pub supported_networks: Vec<String>,
}

pub async fn dips_info(State(state): State<DipsInfo>) -> Json<DipsInfo> {
    Json(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;

    fn sample_state() -> DipsInfo {
        DipsInfo {
            min_grt_per_30_days: BTreeMap::from_iter([
                ("arbitrum-one".to_string(), "450".to_string()),
                ("mainnet".to_string(), "45".to_string()),
            ]),
            min_grt_per_billion_entities_per_30_days: "200".to_string(),
            supported_networks: vec!["arbitrum-one".to_string(), "mainnet".to_string()],
        }
    }

    #[tokio::test]
    async fn returns_state_verbatim() {
        // Arrange
        let state = sample_state();
        let expected = state.clone();

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert_eq!(response.min_grt_per_30_days, expected.min_grt_per_30_days);
        assert_eq!(
            response.min_grt_per_billion_entities_per_30_days,
            expected.min_grt_per_billion_entities_per_30_days
        );
        assert_eq!(response.supported_networks, expected.supported_networks);
    }

    #[tokio::test]
    async fn empty_state_renders_empty_lists_and_zero() {
        // Arrange: indexer has [dips] configured but priced nothing.
        let state = DipsInfo {
            min_grt_per_30_days: BTreeMap::new(),
            min_grt_per_billion_entities_per_30_days: "0".to_string(),
            supported_networks: vec![],
        };

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert!(response.min_grt_per_30_days.is_empty());
        assert!(response.supported_networks.is_empty());
        assert_eq!(response.min_grt_per_billion_entities_per_30_days, "0");
    }

    #[test]
    fn json_shape_is_flat_with_three_top_level_fields() {
        // Arrange
        let state = sample_state();

        // Act
        let json = serde_json::to_value(&state).expect("serializes");

        // Assert: three top-level keys, no `pricing` nesting.
        let obj = json.as_object().expect("object");
        assert!(obj.contains_key("min_grt_per_30_days"));
        assert!(obj.contains_key("min_grt_per_billion_entities_per_30_days"));
        assert!(obj.contains_key("supported_networks"));
        assert_eq!(obj.len(), 3);
        assert!(!obj.contains_key("pricing"));
    }
}
