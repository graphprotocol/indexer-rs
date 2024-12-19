// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;

use axum::{routing::get, serve, Router};
use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_histogram_vec, CounterVec, HistogramVec, TextEncoder,
};
use reqwest::StatusCode;
use tokio::net::TcpListener;

lazy_static! {
    /// Metric registered in global registry for
    /// indexer query handler
    ///
    /// Labels: "deployment", "allocation", "sender"
    pub static ref HANDLER_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "indexer_query_handler_seconds",
        "Histogram for default indexer query handler",
        &["deployment", "allocation", "sender", "status_code"]
    ).unwrap();

    /// Metric registered in global registry for
    /// Failed receipt checks
    ///
    /// Labels: "deployment", "allocation", "sender"
    pub static ref FAILED_RECEIPT: CounterVec = register_counter_vec!(
        "indexer_receipt_failed_total",
        "Failed receipt checks",
        &["deployment", "allocation", "sender"]
    )
    .unwrap();

}

pub fn serve_metrics(host_and_port: SocketAddr) {
    tracing::info!(address = %host_and_port, "Serving prometheus metrics");

    tokio::spawn(async move {
        let router = Router::new().route(
            "/metrics",
            get(|| async {
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
            }),
        );

        serve(
            TcpListener::bind(host_and_port)
                .await
                .expect("Failed to bind to metrics port"),
            router.into_make_service(),
        )
        .await
        .expect("Failed to serve metrics")
    });
}
