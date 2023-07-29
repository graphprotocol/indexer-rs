use axum::{extract::Extension, routing::get, Router};
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use serde_json::json;

use crate::server::ServerOptions;

#[derive(Serialize)]
struct Health {
    healthy: bool,
}

/// Endpoint for server health
pub async fn health() -> impl IntoResponse {
    let health = Health { healthy: true };
    (StatusCode::OK, Json(health))
}

/// Index endpoint for status checks
pub async fn index() -> impl IntoResponse {
    let responder = "Ready to roll!".to_string();
    responder.into_response()
}

/// Endpoint for package version
pub async fn version(server: axum::extract::Extension<ServerOptions>) -> impl IntoResponse {
    let version = server.release.clone();
    (StatusCode::OK, Json(version))
}

// Define a handler function for the `/info` route
async fn operator_info(Extension(options): Extension<ServerOptions>) -> Json<serde_json::Value> {
    let public_key = &options.operator_public_key;
    Json(json!({ "publicKey": public_key }))
}

// Create a function to build the operator server router
pub fn create_operator_server(_options: ServerOptions) -> Router {
    println!("create_operator_server?");
    Router::new().route("/info", get(operator_info))
}
