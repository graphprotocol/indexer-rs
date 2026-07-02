// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Prometheus metrics for the DIPs proposal path. Registered in the global
//! registry, so the indexer-service /metrics endpoint exposes them.

use std::sync::LazyLock;

use prometheus::{register_counter_vec, register_int_gauge, CounterVec, IntGauge};

/// Incoming agreement proposals by outcome: `accepted`, `untrusted` (signer is
/// not a role holder), `transient` (couldn't verify -- sender should retry), or
/// `rejected` (other validation failure).
pub static PROPOSAL_OUTCOMES: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "dips_proposal_outcomes_total",
        "Incoming DIPs agreement proposals by outcome",
        &["outcome"]
    )
    .unwrap()
});

/// Agreement-manager role holders in the last successful subgraph fetch.
pub static ROLE_SET_SIZE: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "dips_agreement_manager_role_holders",
        "Agreement-manager role holders in the last successful subgraph fetch"
    )
    .unwrap()
});

/// Unix time of the last successful role-set fetch. Alert on `time() - this` to
/// catch a refresh that has stalled.
pub static ROLE_LAST_REFRESH_TIMESTAMP: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "dips_agreement_manager_role_last_refresh_timestamp",
        "Unix time of the last successful agreement-manager role fetch"
    )
    .unwrap()
});
