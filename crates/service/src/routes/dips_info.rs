// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// State for the /dips/info endpoint. `Failed` deliberately carries no error
/// detail: init errors can reference the operator's RPC endpoint (which may
/// embed credentials), so the cause stays in logs and metrics only.
#[derive(Clone, Debug)]
pub enum DipsInfoState {
    Available {
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
            min_grt_per_30_days,
            min_grt_per_billion_entities_per_30_days,
        } => {
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
            min_grt_per_30_days: BTreeMap::from([("mainnet".to_string(), "450".to_string())]),
            min_grt_per_billion_entities_per_30_days: "200".to_string(),
        };

        // Act
        let response = dips_info(State(state)).await;

        // Assert
        assert_eq!(response.status(), StatusCode::OK);
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
