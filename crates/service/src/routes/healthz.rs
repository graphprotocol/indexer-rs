// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use reqwest::Url;
use serde_json::json;

/// State required by the `/healthz` endpoint for performing health checks.
///
/// This state is injected via Axum's `State` extractor and provides access to
/// the resources needed to verify system health:
/// - Database connectivity (via connection pool)
/// - Graph-node availability (via HTTP client and status URL)
///
/// # Initialization Order
/// Must be constructed after the database pool and HTTP client are initialized.
/// Typically created during router setup in [`crate::service::router::ServiceRouter`].
#[derive(Clone)]
pub struct HealthzState {
    /// PostgreSQL connection pool for database health checks.
    pub db: sqlx::PgPool,
    /// Shared HTTP client for making health check requests to graph-node.
    pub http_client: reqwest::Client,
    /// URL of the graph-node status endpoint (GraphQL).
    pub graph_node_status_url: Url,
}

pub async fn healthz(State(state): State<HealthzState>) -> impl IntoResponse {
    let mut ok = true;
    let mut checks = serde_json::Map::new();

    // Database health
    match tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::query("SELECT 1").execute(&state.db),
    )
    .await
    {
        Ok(Ok(_)) => {
            checks.insert("database".into(), json!({ "status": "healthy" }));
        }
        Ok(Err(err)) => {
            ok = false;
            checks.insert(
                "database".into(),
                json!({ "status": "unhealthy", "error": err.to_string() }),
            );
        }
        Err(_) => {
            ok = false;
            checks.insert(
                "database".into(),
                json!({ "status": "unhealthy", "error": "timeout" }),
            );
        }
    }

    // Graph-node health (status endpoint is GraphQL)
    let graph_node_query = json!({
        "query": "{ __typename }"
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
                        "status": "unhealthy",
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
                                json!({ "status": "unhealthy", "error": "graphql errors returned" }),
                            );
                        } else {
                            checks.insert("graph_node".into(), json!({ "status": "healthy" }));
                        }
                    }
                    Err(err) => {
                        ok = false;
                        checks.insert(
                            "graph_node".into(),
                            json!({ "status": "unhealthy", "error": err.to_string() }),
                        );
                    }
                }
            }
        }
        Err(err) => {
            ok = false;
            checks.insert(
                "graph_node".into(),
                json!({ "status": "unhealthy", "error": err.to_string() }),
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
