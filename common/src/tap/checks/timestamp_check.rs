// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use std::time::{Duration, SystemTime};

pub struct TimestampCheck {
    timestamp_threshold: Duration,
}

use tap_core::receipt::{
    checks::{Check, CheckResult},
    Checking, ReceiptWithState,
};

impl TimestampCheck {
    pub fn new(timestamp_threshold: Duration) -> Self {
        Self {
            timestamp_threshold,
        }
    }
}

#[async_trait::async_trait]
impl Check for TimestampCheck {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult {
        let timestamp_now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
        let min_timestamp = timestamp_now - self.timestamp_threshold;
        let max_timestamp = timestamp_now + self.timestamp_threshold;

        let receipt_timestamp = Duration::from_nanos(receipt.signed_receipt().message.timestamp_ns);

        if receipt_timestamp < max_timestamp && receipt_timestamp > min_timestamp {
            Ok(())
        } else {
            Err(anyhow!(
                "Receipt timestamp `{}` is outside threshold",
                receipt_timestamp.as_secs()
            ))
        }
    }
}
