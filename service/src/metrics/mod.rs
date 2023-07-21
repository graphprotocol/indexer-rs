use autometrics::{encode_global_metrics, global_metrics_exporter};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use once_cell::sync::Lazy;
use prometheus::{core::Collector, Registry};
use prometheus::{linear_buckets, HistogramOpts, HistogramVec, IntCounterVec, Opts};
use std::{net::SocketAddr, str::FromStr};
use tracing::{debug, info};

pub static QUERIES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("queries", "Incoming queries")
            .namespace("indexer")
            .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create queries counters");
    prometheus::register(Box::new(m.clone())).expect("Failed to register queries counter");
    m
});

pub static SUCCESSFUL_QUERIES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("successfulQueries", "Successfully executed queries")
            .namespace("indexer")
            .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create successfulQueries counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register successfulQueries counter");
    m
});

pub static FAILED_QUERIES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("failedQueries", "Queries that failed to execute")
            .namespace("indexer")
            .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create failedQueries counters");
    prometheus::register(Box::new(m.clone())).expect("Failed to register failedQueries counter");
    m
});

pub static QUERIES_WITH_INVALID_RECEIPT_HEADER: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new(
            "queriesWithInvalidReceiptHeader",
            "Queries that failed executing because they came with an invalid receipt header",
        )
        .namespace("indexer")
        .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create queriesWithInvalidReceiptHeader counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register queriesWithInvalidReceiptHeader counter");
    m
});

pub static QUERIES_WITH_INVALID_RECEIPT_VALUE: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new(
            "queriesWithInvalidReceiptValue",
            "Queries that failed executing because they came with an invalid receipt value",
        )
        .namespace("indexer")
        .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create queriesWithInvalidReceiptValue counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register queriesWithInvalidReceiptValue counter");
    m
});

pub static QUERIES_WITHOUT_RECEIPT: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new(
            "queriesWithoutReceipt",
            "Queries that failed executing because they came without a receipt",
        )
        .namespace("indexer")
        .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create queriesWithoutReceipt counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register queriesWithoutReceipt counter");
    m
});

pub static CHANNEL_MESSAGES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("channelMessages", "Incoming channel messages")
            .namespace("indexer")
            .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create channelMessages counters");
    prometheus::register(Box::new(m.clone())).expect("Failed to register channelMessages counter");
    m
});

pub static SUCCESSFUL_CHANNEL_MESSAGES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new(
            "successfulChannelMessages",
            "Successfully handled channel messages",
        )
        .namespace("indexer")
        .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create successfulChannelMessages counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register successfulChannelMessages counter");
    m
});

pub static FAILED_CHANNEL_MESSAGES: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("failedChannelMessages", "Failed channel messages")
            .namespace("indexer")
            .subsystem("service"),
        &["deployment"],
    )
    .expect("Failed to create failedChannelMessages counters");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register failedChannelMessages counter");
    m
});

#[allow(dead_code)]
pub static QUERY_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let m = HistogramVec::new(
        HistogramOpts::new(
            "query_duration",
            "Duration of processing a query from start to end",
        )
        .namespace("indexer")
        .subsystem("service")
        .buckets(linear_buckets(0.0, 1.0, 20).unwrap()),
        &["deployment", "name"],
    )
    .expect("Failed to create query_duration histograms");
    prometheus::register(Box::new(m.clone())).expect("Failed to register query_duration counter");
    m
});

#[allow(dead_code)]
pub static CHANNEL_MESSAGE_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let m = HistogramVec::new(
        HistogramOpts::new(
            "channel_message_duration",
            "Duration of processing channel messages",
        )
        .namespace("indexer")
        .subsystem("service")
        .buckets(linear_buckets(0.0, 1.0, 20).unwrap()),
        &["deployment"],
    )
    .expect("Failed to create channel_message_duration histograms");
    prometheus::register(Box::new(m.clone()))
        .expect("Failed to register channel_message_duration counter");
    m
});

#[allow(dead_code)]
pub static INDEXER_ERROR: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("indexer_error", "Indexer errors observed over time")
            .namespace("indexer")
            .subsystem("service"),
        &["code"],
    )
    .expect("Failed to create indexer_error");
    prometheus::register(Box::new(m.clone())).expect("Failed to register indexer_error counter");
    m
});

#[allow(dead_code)]
pub static REGISTRY: Lazy<prometheus::Registry> = Lazy::new(prometheus::Registry::new);

#[allow(dead_code)]
pub fn register_metrics(registry: &Registry, metrics: Vec<Box<dyn Collector>>) {
    for metric in metrics {
        registry.register(metric).expect("Cannot register metrics");
        debug!("registered metric");
    }
}

/// Start the basic metrics for indexer services
///
#[allow(dead_code)]
pub fn start_metrics() {
    register_metrics(
        &REGISTRY,
        vec![
            Box::new(QUERIES.clone()),
            Box::new(SUCCESSFUL_QUERIES.clone()),
            Box::new(FAILED_QUERIES.clone()),
            Box::new(QUERIES_WITH_INVALID_RECEIPT_HEADER.clone()),
            Box::new(QUERIES_WITH_INVALID_RECEIPT_VALUE.clone()),
            Box::new(QUERIES_WITHOUT_RECEIPT.clone()),
            Box::new(CHANNEL_MESSAGES.clone()),
            Box::new(SUCCESSFUL_CHANNEL_MESSAGES.clone()),
            Box::new(FAILED_CHANNEL_MESSAGES.clone()),
            Box::new(QUERY_DURATION.clone()),
            Box::new(CHANNEL_MESSAGE_DURATION.clone()),
            Box::new(INDEXER_ERROR.clone()),
        ],
    );
}

/// This handler serializes the metrics into a string for Prometheus to scrape
#[allow(dead_code)]
pub async fn get_metrics() -> (StatusCode, String) {
    match encode_global_metrics() {
        Ok(metrics) => (StatusCode::OK, metrics),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:?}")),
    }
}

/// Run the API server as well as Prometheus and a traffic generator
#[allow(dead_code)]
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
        .expect("Error starting example API server");
}
