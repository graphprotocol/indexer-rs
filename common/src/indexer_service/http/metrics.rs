// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use prometheus::{register_int_counter_vec, IntCounterVec};

pub struct IndexerServiceMetrics {
    pub requests: IntCounterVec,
    pub successful_requests: IntCounterVec,
    pub failed_requests: IntCounterVec,
}

impl IndexerServiceMetrics {
    pub fn new(prefix: &str) -> Self {
        IndexerServiceMetrics {
            requests: register_int_counter_vec!(
                format!("{prefix}_service_requests_total"),
                "Incoming requests",
                &["manifest"]
            )
            .unwrap(),

            successful_requests: register_int_counter_vec!(
                format!("{prefix}_service_requests_ok"),
                "Successfully executed requests",
                &["manifest"]
            )
            .unwrap(),

            failed_requests: register_int_counter_vec!(
                format!("{prefix}_service_requests_failed"),
                "requests that failed to execute",
                &["manifest"]
            )
            .unwrap(),
        }
    }
}
