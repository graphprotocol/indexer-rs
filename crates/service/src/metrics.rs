// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{net::SocketAddr, sync::LazyLock};

use axum::{routing::get, serve, Router};
use prometheus::{
    register_counter_vec, register_histogram_vec, register_int_gauge, CounterVec, HistogramVec,
    IntGauge, TextEncoder,
};
use reqwest::StatusCode;
use tokio::net::TcpListener;

/// Metric registered in global registry for
/// indexer query handler
///
/// Labels: "deployment", "allocation", "sender"
pub static HANDLER_HISTOGRAM: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "indexer_query_handler_seconds",
        "Histogram for default indexer query handler",
        &["deployment", "allocation", "sender", "status_code"]
    )
    .unwrap()
});

/// Metric registered in global registry for
/// Failed receipt checks
///
/// Labels: "deployment", "allocation", "sender"
pub static FAILED_RECEIPT: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "indexer_receipt_failed_total",
        "Failed receipt checks",
        &["deployment", "allocation", "sender"]
    )
    .unwrap()
});

/// Set to 1 when the DIPs gRPC server started, 0 when DIPs was configured but
/// failed to initialize (the service then runs without DIPs). Not exported
/// when DIPs is not configured, so alerting on 0 is safe for DIPs operators.
pub static DIPS_ENABLED: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "indexer_dips_enabled",
        "1 if the DIPs gRPC server is running; 0 if DIPs is configured but failed to initialize"
    )
    .unwrap()
});

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
                        tracing::error!(error = %e, "Error encoding metrics");
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Error encoding metrics: {e}"),
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
