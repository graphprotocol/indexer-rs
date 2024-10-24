use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use tokio::net::TcpListener;

use crate::subgraph::{network_subgraph, NETWORK_SUBGRAPH_ROUTE};

const HOST: &str = "0.0.0.0";
const PORT: &str = "8000";

pub async fn start_server() -> anyhow::Result<()> {
    let port = dotenvy::var("API_PORT").unwrap_or(PORT.into());
    let listener = TcpListener::bind(&format!("{HOST}:{port}")).await?;

    let router = Router::new()
        .route("/health", get(health_check))
        .route(NETWORK_SUBGRAPH_ROUTE, post(network_subgraph));

    Ok(axum::serve(listener, router).await?)
}

async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}
