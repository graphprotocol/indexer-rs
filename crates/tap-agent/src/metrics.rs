// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, panic};

use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
use futures_util::FutureExt;
use prometheus::TextEncoder;

async fn handler_metrics() -> (StatusCode, String) {
    let metric_families = prometheus::gather();
    let encoder = TextEncoder::new();

    match encoder.encode_to_string(&metric_families) {
        Ok(s) => (StatusCode::OK, s),
        Err(e) => {
            tracing::error!("Error encoding metrics: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error encoding metrics: {}", e),
            )
        }
    }
}

async fn handler_404() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "404 Not Found")
}

async fn _run_server(port: u16) {
    let app = Router::new()
        .route("/metrics", get(handler_metrics))
        .fallback(handler_404);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to Bind metrics address`");
    let server = axum::serve(listener, app.into_make_service());

    tracing::info!("Metrics server listening on {}", addr);

    let res = server.await;

    tracing::debug!("Metrics server stopped");

    if let Err(err) = res {
        panic!("Metrics server error: {:#?}", err);
    };
}

pub async fn run_server(port: u16) {
    // Code here is to abort program if there is a panic in _run_server
    // Otherwise, when spawning the task, the panic will be silently ignored
    let res = panic::AssertUnwindSafe(_run_server(port))
        .catch_unwind()
        .await;
    if res.is_err() {
        std::process::abort();
    }
}
