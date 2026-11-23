// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// State for the /dips/info endpoint. `Failed` deliberately carries no error
/// detail: init errors can reference the operator's RPC endpoint (which may
/// embed credentials), so the cause stays in logs and metrics only.
#[derive(Clone, Debug)]
pub enum DipsInfoState {
    Available {
        supported_networks: HashSet<String>,
        min_grt_per_30_days: BTreeMap<String, String>,
        min_grt_per_billion_entities_per_30_days: String,
    },
    Failed,
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

#[derive(Serialize)]
pub struct DipsInfoUnavailable {
    pub status: &'static str,
    pub error: &'static str,
}

pub async fn dips_info(State(state): State<DipsInfoState>) -> Response {
    match state {
        DipsInfoState::Available {
            supported_networks,
            mut min_grt_per_30_days,
            min_grt_per_billion_entities_per_30_days,
        } => {
            // Match proposal acceptance: a network must be enabled AND priced.
            // Filter prices too: selection clients may infer support from their keys
            // when the explicit support list is empty.
            min_grt_per_30_days.retain(|network, _| supported_networks.contains(network));
            let supported_networks: Vec<String> = min_grt_per_30_days.keys().cloned().collect();

            Json(DipsInfoResponse {
                pricing: DipsInfoPricing {
                    min_grt_per_30_days,
                    min_grt_per_billion_entities_per_30_days,
                },
                supported_networks,
            })
            .into_response()
        }
        DipsInfoState::Failed => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(DipsInfoUnavailable {
                status: "unavailable",
                error: "DIPs initialization failed; check the indexer-service logs",
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dips_info_available_returns_pricing() {
        // Arrange
        let state = DipsInfoState::Available {
            supported_networks: HashSet::from(["mainnet".to_string()]),
            min_grt_per_30_days: BTreeMap::from([("mainnet".to_string(), "450".to_string())]),
            min_grt_per_billion_entities_per_30_days: "200".to_string(),
        };

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert_eq!(response.status(), StatusCode::OK);
    }

    async fn advertised(enabled: &[&str], priced: &[&str]) -> serde_json::Value {
        let state = DipsInfoState::Available {
            supported_networks: enabled.iter().map(|n| n.to_string()).collect(),
            min_grt_per_30_days: priced
                .iter()
                .map(|n| (n.to_string(), "450".into()))
                .collect(),
            min_grt_per_billion_entities_per_30_days: "200".into(),
        };
        let response = dips_info(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn advertises_only_enabled_and_priced_networks() {
        let data = advertised(&["mainnet", "sepolia"], &["mainnet", "arbitrum-one"]).await;
        assert_eq!(data["supported_networks"], serde_json::json!(["mainnet"]));
        assert_eq!(
            data["pricing"]["min_grt_per_30_days"],
            serde_json::json!({"mainnet": "450"})
        );
        assert_eq!(
            data["pricing"]["min_grt_per_billion_entities_per_30_days"],
            "200"
        );
    }

    #[tokio::test]
    async fn empty_intersection_does_not_advertise_prices_for_fallback_discovery() {
        for (enabled, priced) in [
            (vec![], vec!["mainnet"]),
            (vec!["mainnet"], vec!["sepolia"]),
            (vec!["mainnet"], vec![]),
        ] {
            let data = advertised(&enabled, &priced).await;
            assert_eq!(data["supported_networks"], serde_json::json!([]));
            assert_eq!(
                data["pricing"]["min_grt_per_30_days"],
                serde_json::json!({})
            );
        }
    }

    #[tokio::test]
    async fn test_dips_info_failed_returns_service_unavailable() {
        // Arrange
        let state = DipsInfoState::Failed;

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
