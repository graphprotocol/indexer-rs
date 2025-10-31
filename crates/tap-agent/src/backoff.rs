// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # backoff
//!
//! Helper for tracking exponential backoff windows without blocking the async actor loop. Actors
//! process one message at a time, so we just mark the next instant when work is allowed again and
//! query that metadata before firing a request.

use std::time::{Duration, Instant};

/// Backoff information based on [`Instant`]
#[derive(Debug, Clone)]
pub struct BackoffInfo {
    failed_count: u32,
    failed_backoff_time: Instant,
}

impl BackoffInfo {
    /// Marks a successful attempt, resetting counters and clearing any pending backoff delay.
    pub fn ok(&mut self) {
        self.failed_count = 0;
        self.failed_backoff_time = Instant::now();
    }

    /// Marks a failed attempt, growing the backoff delay exponentially up to 60 seconds.
    pub fn fail(&mut self) {
        let delay =
            (Duration::from_millis(100) * 2u32.pow(self.failed_count)).min(Duration::from_secs(60));
        self.failed_backoff_time = Instant::now() + delay;
        self.failed_count += 1;
    }

    /// Returns the remaining backoff duration, if the current attempt should keep waiting.
    pub fn remaining(&self) -> Option<Duration> {
        self.failed_backoff_time
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    /// Returns whether the caller is still inside the backoff window.
    pub fn in_backoff(&self) -> bool {
        self.remaining().is_some()
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
