// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # Adaptative concurrency
//! This module provide [AdaptiveLimiter] as a tool to allow concurrency.
//! It's implemented with a Additive increase, Multiplicative decrease
//! ([AIMD](https://en.wikipedia.org/wiki/Additive_increase/multiplicative_decrease))
//! strategy.
//!
//!
//!
//! This allows us to have a big number of rav requests running
//! concurrently, but in case of any of them fails, we limit
//! the following requests until the aggregator recovers.
//!
//! ## Behaviour
//! On every request, the caller acquires a slot by calling [AdaptiveLimiter::acquire()],
//! this will increment the number of in_flight connections.
//!
//! If we receive a successful response, we increment our limit to be able to process
//! one more request concurrently.
//!
//! If we receive a failed response, we decrement our limit by half so quickly
//! relieve the pressure in the system.

use std::ops::Range;

/// Simple struct that keeps track of concurrent requests
///
/// More information on [crate::adaptative_concurrency]
pub struct AdaptiveLimiter {
    range: Range<usize>,
    current_limit: usize,
    in_flight: usize,
}

impl AdaptiveLimiter {
    /// Creates an instance of [AdaptiveLimiter] with an `initial_limit`
    /// and a `range` that contains the minimum and maximum of concurrent
    /// requests
    pub fn new(initial_limit: usize, range: Range<usize>) -> Self {
        Self {
            range,
            current_limit: initial_limit,
            in_flight: 0,
        }
    }

    /// Acquires a slot in our limiter, returning `bool`
    /// representing if we had limit available or not
    pub fn acquire(&mut self) -> bool {
        self.has_limit() && {
            self.in_flight += 1;
            true
        }
    }

    /// Returns if there're slots available
    pub fn has_limit(&self) -> bool {
        self.in_flight < self.current_limit
    }

    /// Callback function that removes in_flight counter
    /// and if the current limit is lower than the provided
    /// limit, increase the current limit by 1.
    pub fn on_success(&mut self) {
        self.in_flight -= 1;
        if self.current_limit < self.range.end {
            self.current_limit += 1; // Additive Increase
        }
    }

    /// Callback function that removes in_flight counter
    /// and decreasing the current limit by half, with
    /// minimum value to configured value.
    pub fn on_failure(&mut self) {
        // Multiplicative Decrease
        self.in_flight -= 1;
        self.current_limit = (self.current_limit / 2).max(self.range.start);
    }
}

#[cfg(test)]
mod tests {
    use super::AdaptiveLimiter;

    #[test]
    fn test_adaptative_concurrency() {
        let mut limiter = AdaptiveLimiter::new(2, 1..10);
        assert_eq!(limiter.current_limit, 2);
        assert_eq!(limiter.in_flight, 0);

        assert!(limiter.acquire());
        assert!(limiter.acquire());
        assert!(!limiter.acquire());
        assert_eq!(limiter.in_flight, 2);

        limiter.on_success();
        assert_eq!(limiter.in_flight, 1);
        assert_eq!(limiter.current_limit, 3);
        limiter.on_success();
        assert_eq!(limiter.in_flight, 0);
        assert_eq!(limiter.current_limit, 4);

        assert!(limiter.acquire());
        assert!(limiter.acquire());
        assert!(limiter.acquire());
        assert!(limiter.acquire());
        assert!(!limiter.acquire());
        assert_eq!(limiter.in_flight, 4);
        assert_eq!(limiter.current_limit, 4);

        limiter.on_failure();
        assert_eq!(limiter.current_limit, 2);
        assert_eq!(limiter.in_flight, 3);
        limiter.on_success();
        assert_eq!(limiter.current_limit, 3);
        assert_eq!(limiter.in_flight, 2);

        assert!(limiter.acquire());
        assert!(!limiter.acquire());
    }
}
