// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{extract::State, Json};
use serde::Serialize;
use std::collections::BTreeMap;

/// State for the /dips/info endpoint, derived from DipsConfig at startup.
#[derive(Clone, Debug)]
pub struct DipsInfoState {
    pub min_grt_per_30_days: BTreeMap<String, String>,
    pub min_grt_per_million_entities_per_30_days: String,
}

#[derive(Serialize)]
pub struct DipsInfoPricing {
    pub min_grt_per_30_days: BTreeMap<String, String>,
    pub min_grt_per_million_entities_per_30_days: String,
}

#[derive(Serialize)]
pub struct DipsInfoResponse {
    pub pricing: DipsInfoPricing,
    pub supported_networks: Vec<String>,
    pub dips_version: &'static str,
}

pub async fn dips_info(State(state): State<DipsInfoState>) -> Json<DipsInfoResponse> {
    let supported_networks: Vec<String> = state.min_grt_per_30_days.keys().cloned().collect();

    Json(DipsInfoResponse {
        pricing: DipsInfoPricing {
            min_grt_per_30_days: state.min_grt_per_30_days,
            min_grt_per_million_entities_per_30_days: state
                .min_grt_per_million_entities_per_30_days,
        },
        supported_networks,
        dips_version: "2",
    })
}
