// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use once_cell::sync::Lazy;
use prometheus::{core::Collector, register, IntCounterVec, Opts, Registry};

#[allow(dead_code)]
pub static INDEXER_ERROR: Lazy<IntCounterVec> = Lazy::new(|| {
    let m = IntCounterVec::new(
        Opts::new("indexer_error", "Indexer errors observed over time")
            .namespace("indexer")
            .subsystem("service"),
        &["code"],
    )
    .expect("Failed to create indexer_error");
    register(Box::new(m.clone())).expect("Failed to register indexer_error counter");
    m
});

#[allow(dead_code)]
pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

#[allow(dead_code)]
pub fn register_metrics(registry: &Registry, metrics: Vec<Box<dyn Collector>>) {
    for metric in metrics {
        registry.register(metric).expect("Cannot register metrics");
    }
}

/// Register indexer error metrics in Prometheus registry
pub fn register_indexer_error_metrics() {
    register_metrics(&REGISTRY, vec![Box::new(INDEXER_ERROR.clone())]);
}
