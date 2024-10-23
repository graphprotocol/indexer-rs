use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BackoffInfo {
    failed_count: u32,
    failed_backoff_time: Instant,
}

impl BackoffInfo {
    pub fn ok(&mut self) {
        self.failed_count = 0;
    }

    pub fn fail(&mut self) {
        // backoff = max(100ms * 2 ^ retries, 60s)
        self.failed_backoff_time = Instant::now()
            + (Duration::from_millis(100) * 2u32.pow(self.failed_count))
                .min(Duration::from_secs(60));
        self.failed_count += 1;
    }

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
