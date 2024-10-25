// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::ops::Range;

pub struct AdaptiveLimiter {
    range: Range<usize>,
    current_limit: usize,
    in_flight: usize,
}

impl AdaptiveLimiter {
    pub fn new(initial_limit: usize, range: Range<usize>) -> Self {
        Self {
            range,
            current_limit: initial_limit,
            in_flight: 0,
        }
    }

    pub fn acquire(&mut self) -> bool {
        self.has_limit() && {
            self.in_flight += 1;
            true
        }
    }

    pub fn has_limit(&self) -> bool {
        self.in_flight < self.current_limit
    }

    pub fn on_success(&mut self) {
        self.in_flight -= 1;
        if self.current_limit < self.range.end {
            self.current_limit += 1; // Additive Increase
        }
    }

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
