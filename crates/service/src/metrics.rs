use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};

lazy_static! {
    /// Register indexer error metrics in Prometheus registry
    pub static ref HANDLER_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "indexer_query_handler_seconds",
        "Histogram for default indexer query handler",
        &["deployment", "allocation", "sender"]
    ).unwrap();

    pub static ref HANDLER_FAILURE: CounterVec = register_counter_vec!(
        "indexer_query_handler_failed_total",
        "Failed queries to handler",
        &["deployment"]
    ).unwrap();

    pub static ref FAILED_RECEIPT: CounterVec = register_counter_vec!(
        "indexer_receipt_failed_total",
        "Failed receipt checks",
        &["deployment", "allocation", "sender"]
    ).unwrap();

    pub static ref COST_MODEL_METRIC: HistogramVec = register_histogram_vec!(
        "indexer_cost_model_seconds",
        "Histogram metric for single cost model query",
        &["deployment"]
    )
    .unwrap();
    pub static ref COST_MODEL_FAILED: CounterVec = register_counter_vec!(
        "indexer_cost_model_failed_total",
        "Total failed Cost Model query",
        &["deployment"]
    )
    .unwrap();
    pub static ref COST_MODEL_INVALID: Counter = register_counter!(
        "indexer_cost_model_invalid_total",
        "Cost model queries with invalid deployment id",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_METRIC: Histogram = register_histogram!(
        "indexer_cost_model_batch_seconds",
        "Histogram metric for batch cost model query",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_SIZE: Histogram = register_histogram!(
        "indexer_cost_model_batch_size",
        "This shows the size of deployment ids cost model batch queries got",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_FAILED: Counter = register_counter!(
        "indexer_cost_model_batch_failed_total",
        "Total failed batch cost model queries",
    )
    .unwrap();
    pub static ref COST_MODEL_BATCH_INVALID: Counter = register_counter!(
        "indexer_cost_model_batch_invalid_total",
        "Batch cost model queries with invalid deployment ids",
    )
    .unwrap();

}

fn serve_metrics(host_and_port: SocketAddr) {
    info!(address = %host_and_port, "Serving prometheus metrics");

    tokio::spawn(async move {
        let router = Router::new().route(
            "/metrics",
            get(|| async {
                let metric_families = prometheus::gather();
                let encoder = TextEncoder::new();

                match encoder.encode_to_string(&metric_families) {
                    Ok(s) => (StatusCode::OK, s),
                    Err(e) => {
                        error!("Error encoding metrics: {}", e);
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
