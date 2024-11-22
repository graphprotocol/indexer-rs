// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use lazy_static::lazy_static;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};

lazy_static! {
    /// Metric registered in global registry for
    /// indexer query handler
    ///
    /// Labels: "deployment", "allocation", "sender"
    pub static ref HANDLER_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "indexer_query_handler_seconds",
        "Histogram for default indexer query handler",
        &["deployment", "allocation", "sender"]
    ).unwrap();

    /// Metric registered in global registry for
    /// Failed queries to handler
    ///
    /// Labels: "deployment"
    pub static ref HANDLER_FAILURE: CounterVec = register_counter_vec!(
        "indexer_query_handler_failed_total",
        "Failed queries to handler",
        &["deployment"]
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
