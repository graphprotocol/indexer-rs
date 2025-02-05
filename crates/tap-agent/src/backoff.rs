// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # backoff
//! This module is used to provide a helper that keep tracks of exponential backoff information in a
//! non-blocking way. This is important since Actors process one message at a time, and an sleep in
//! the middle would affect performance.
//!
//! This way we just mark something as "in backoff" and just check that information before sending
//! the request.
//!
//! This module is also used by [crate::tracker].

use std::time::{Duration, Instant};

/// Backoff information based on [Instant]
#[derive(Debug, Clone)]
pub struct BackoffInfo {
    failed_count: u32,
    failed_backoff_time: Instant,
}

impl BackoffInfo {
    /// Callback represinging that a request succeed
    ///
    /// This resets the failed_count
    pub fn ok(&mut self) {
        self.failed_count = 0;
    }

    /// Callback represinging that a request failed
    ///
    /// This sets the backoff time to max(100ms * 2 ^ retries, 60s)
    pub fn fail(&mut self) {
        // backoff = max(100ms * 2 ^ retries, 60s)
        self.failed_backoff_time = Instant::now()
            + (Duration::from_millis(100) * 2u32.pow(self.failed_count))
                .min(Duration::from_secs(60));
        self.failed_count += 1;
    }

    /// Returns if is in backoff
    pub fn in_backoff(&self) -> bool {
        let now = Instant::now();
        now < self.failed_backoff_time
    }
}

impl Default for BackoffInfo {
    fn default() -> Self {
        Self {
            failed_count: 0,
            failed_backoff_time: Instant::now(),
        }
    }
}
