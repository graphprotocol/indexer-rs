// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Shared counter of in-flight proposal requests.
//!
//! The counter is incremented when a request enters the gRPC handler and
//! decremented when it leaves. The IPFS client reads it at the start of a
//! fetch to decide whether to use the full retry budget or a single
//! attempt — the latter frees handler slots faster when the service is
//! under load, providing a pressure-relief valve.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub type InflightCounter = Arc<AtomicUsize>;

/// RAII guard that increments the counter on construction and decrements
/// on drop. Hold it for the lifetime of the request you want counted.
pub struct InflightGuard {
    counter: InflightCounter,
}

impl InflightGuard {
    pub fn new(counter: InflightCounter) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Snapshot the current in-flight count.
pub fn snapshot(counter: &InflightCounter) -> usize {
    counter.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_increments_and_decrements() {
        let counter: InflightCounter = Arc::new(AtomicUsize::new(0));
        assert_eq!(snapshot(&counter), 0);

        let g1 = InflightGuard::new(counter.clone());
        assert_eq!(snapshot(&counter), 1);

        let g2 = InflightGuard::new(counter.clone());
        assert_eq!(snapshot(&counter), 2);

        drop(g1);
        assert_eq!(snapshot(&counter), 1);

        drop(g2);
        assert_eq!(snapshot(&counter), 0);
    }
}
