// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use reqwest::Url;
use serde_json::json;

#[derive(Clone)]
pub struct HealthzState {
    pub db: sqlx::PgPool,
    pub http_client: reqwest::Client,
    pub graph_node_status_url: Url,
}

pub async fn healthz(State(state): State<HealthzState>) -> impl IntoResponse {
    let mut ok = true;
    let mut checks = serde_json::Map::new();

    // Database health
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => {
            checks.insert("database".into(), json!({ "status": "ok" }));
        }
        Err(err) => {
            ok = false;
            checks.insert(
                "database".into(),
                json!({ "status": "error", "error": err.to_string() }),
            );
        }
    }

    // Graph-node health (status endpoint is GraphQL)
    let graph_node_query = json!({
        "query": "{ indexingStatuses(first: 1) { subgraph } }"
    });
    match state
        .http_client
        .post(state.graph_node_status_url.clone())
        .timeout(Duration::from_secs(5))
        .json(&graph_node_query)
        .send()
        .await
    {
        Ok(response) => {
            if !response.status().is_success() {
                ok = false;
                checks.insert(
                    "graph_node".into(),
                    json!({
                        "status": "error",
                        "error": format!("status {}", response.status())
                    }),
                );
            } else {
                match response.json::<serde_json::Value>().await {
                    Ok(body) => {
                        if body.get("errors").is_some() {
                            ok = false;
                            checks.insert(
                                "graph_node".into(),
                                json!({ "status": "error", "error": "graphql errors returned" }),
                            );
                        } else {
                            checks.insert("graph_node".into(), json!({ "status": "ok" }));
                        }
                    }
                    Err(err) => {
                        ok = false;
                        checks.insert(
                            "graph_node".into(),
                            json!({ "status": "error", "error": err.to_string() }),
                        );
                    }
                }
            }
        }
        Err(err) => {
            ok = false;
            checks.insert(
                "graph_node".into(),
                json!({ "status": "error", "error": err.to_string() }),
            );
        }
    }

    let status = if ok { "healthy" } else { "unhealthy" };
    let http_status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        http_status,
        Json(json!({ "status": status, "checks": checks })),
    )
}
