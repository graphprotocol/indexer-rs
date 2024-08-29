// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use lazy_static::lazy_static;
use prometheus::{register_int_counter_vec, IntCounterVec};

lazy_static! {
    /// Register indexer error metrics in Prometheus registry
    pub static ref INDEXER_ERROR: IntCounterVec = register_int_counter_vec!(
        "indexer_error",
        "Indexer errors observed over time",
        &["code"]
    ).expect("Create indexer_error metrics");
}
