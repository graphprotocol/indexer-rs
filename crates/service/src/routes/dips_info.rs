// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::State, Json};
use serde::Serialize;
use std::collections::BTreeMap;

/// State for the /dips/info endpoint, derived from DipsConfig at startup.
#[derive(Clone, Debug)]
pub struct DipsInfoState {
    pub supported_networks: Vec<String>,
    pub min_grt_per_30_days: BTreeMap<String, String>,
    pub min_grt_per_billion_entities_per_30_days: String,
}

#[derive(Serialize)]
pub struct DipsInfoPricing {
    pub min_grt_per_30_days: BTreeMap<String, String>,
    pub min_grt_per_billion_entities_per_30_days: String,
}

#[derive(Serialize)]
pub struct DipsInfoResponse {
    pub pricing: DipsInfoPricing,
    pub supported_networks: Vec<String>,
}

pub async fn dips_info(State(state): State<DipsInfoState>) -> Json<DipsInfoResponse> {
    Json(DipsInfoResponse {
        pricing: DipsInfoPricing {
            min_grt_per_30_days: state.min_grt_per_30_days,
            min_grt_per_billion_entities_per_30_days: state
                .min_grt_per_billion_entities_per_30_days,
        },
        supported_networks: state.supported_networks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;

    fn sample_state() -> DipsInfoState {
        DipsInfoState {
            supported_networks: vec!["arbitrum-one".to_string(), "mainnet".to_string()],
            min_grt_per_30_days: BTreeMap::from_iter([("mainnet".to_string(), "45".to_string())]),
            min_grt_per_billion_entities_per_30_days: "200".to_string(),
        }
    }

    #[tokio::test]
    async fn returns_supported_networks_from_config_not_map_keys() {
        // Arrange: arbitrum-one is declared as supported but has no price entry.
        let state = sample_state();

        // Act
        let response = dips_info(State(state)).await;

        // Assert: both networks appear, even though only mainnet is priced.
        assert_eq!(
            response.supported_networks,
            vec!["arbitrum-one".to_string(), "mainnet".to_string()]
        );
    }

    #[tokio::test]
    async fn surfaces_pricing_from_state() {
        // Arrange
        let state = sample_state();

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert_eq!(
            response.pricing.min_grt_per_30_days.get("mainnet"),
            Some(&"45".to_string())
        );
        assert_eq!(
            response.pricing.min_grt_per_billion_entities_per_30_days,
            "200"
        );
    }

    #[tokio::test]
    async fn empty_state_renders_empty_lists_and_zero() {
        // Arrange: indexer has [dips] configured but populated nothing.
        let state = DipsInfoState {
            supported_networks: vec![],
            min_grt_per_30_days: BTreeMap::new(),
            min_grt_per_billion_entities_per_30_days: "0".to_string(),
        };

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert!(response.supported_networks.is_empty());
        assert!(response.pricing.min_grt_per_30_days.is_empty());
        assert_eq!(
            response.pricing.min_grt_per_billion_entities_per_30_days,
            "0"
        );
    }
}
