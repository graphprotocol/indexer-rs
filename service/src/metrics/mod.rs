// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use autometrics::{encode_global_metrics, global_metrics_exporter};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use lazy_static::lazy_static;
use prometheus::{register_histogram_vec, register_int_counter_vec, HistogramVec, IntCounterVec};
use std::{net::SocketAddr, str::FromStr};
use tracing::info;

// Record Queries related metrics
lazy_static! {
    pub static ref QUERIES: IntCounterVec = register_int_counter_vec!(
        "indexer_service_queries_total",
        "Incoming queries",
        &["deployment"],
    )
    .expect("Failed to create queries counters");
    pub static ref SUCCESSFUL_QUERIES: IntCounterVec = register_int_counter_vec!(
        "indexer_service_queries_ok",
        "Successfully executed queries",
        &["deployment"],
    )
    .expect("Failed to create successfulQueries counters");
    pub static ref FAILED_QUERIES: IntCounterVec = register_int_counter_vec!(
        "indexer_service_queries_failed",
        "Queries that failed to execute",
        &["deployment"],
    )
    .expect("Failed to create failedQueries counters");
    pub static ref QUERIES_WITH_INVALID_RECEIPT_HEADER: IntCounterVec = register_int_counter_vec!(
        "indexer_service_queries_with_invalid_receipt_header",
        "Queries that failed executing because they came with an invalid receipt header",
        &["deployment"],
    )
    .expect("Failed to create queriesWithInvalidReceiptHeader counters");
    pub static ref QUERIES_WITHOUT_RECEIPT: IntCounterVec = register_int_counter_vec!(
        "indexer_service_queries_without_receipt",
        "Queries that failed executing because they came without a receipt",
        &["deployment"],
    )
    .expect("Failed to create queriesWithoutReceipt counters");
    pub static ref QUERY_DURATION: HistogramVec = register_histogram_vec!(
        "indexer_service_query_duration",
        "Duration of processing a query from start to end",
        &["deployment"],
    )
    .unwrap();
}

/// This handler serializes the metrics into a string for Prometheus to scrape
pub async fn get_metrics() -> (StatusCode, String) {
    match encode_global_metrics() {
        Ok(metrics) => (StatusCode::OK, metrics),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:?}")),
    }
}

/// Metrics server router
pub async fn handle_serve_metrics(host: String, port: u16) {
    // Set up the exporter to collect metrics
    let _exporter = global_metrics_exporter();

    let app = Router::new().route("/metrics", get(get_metrics));
    let addr =
        SocketAddr::from_str(&format!("{}:{}", host, port)).expect("Start Prometheus metrics");
    let server = axum::Server::bind(&addr);
    info!(
        address = addr.to_string(),
        "Prometheus Metrics port exposed"
    );

    server
        .serve(app.into_make_service())
        .await
        .expect("Error starting Prometheus metrics port");
}
