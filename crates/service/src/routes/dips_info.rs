// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::State, Json};
use serde::Serialize;
use std::collections::BTreeMap;

/// State for the /dips/info endpoint, derived from DipsConfig at startup.
#[derive(Clone, Debug)]
pub struct DipsInfoState {
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
    // Keys of the price map are the indexer's declared supported chains.
    // Pricing a chain in config is the same act as declaring support for it.
    let supported_networks: Vec<String> = state.min_grt_per_30_days.keys().cloned().collect();

    Json(DipsInfoResponse {
        pricing: DipsInfoPricing {
            min_grt_per_30_days: state.min_grt_per_30_days,
            min_grt_per_billion_entities_per_30_days: state
                .min_grt_per_billion_entities_per_30_days,
        },
        supported_networks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;

    fn sample_state() -> DipsInfoState {
        DipsInfoState {
            min_grt_per_30_days: BTreeMap::from_iter([
                ("arbitrum-one".to_string(), "450".to_string()),
                ("mainnet".to_string(), "45".to_string()),
            ]),
            min_grt_per_billion_entities_per_30_days: "200".to_string(),
        }
    }

    #[tokio::test]
    async fn supported_networks_derived_from_price_map_keys() {
        // Arrange
        let state = sample_state();

        // Act
        let response = dips_info(State(state)).await;

        // Assert: BTreeMap iteration is sorted, so output is deterministic.
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
        // Arrange: indexer has [dips] configured but priced nothing.
        let state = DipsInfoState {
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
