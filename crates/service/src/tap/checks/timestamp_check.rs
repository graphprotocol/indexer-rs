// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0
use std::time::{Duration, SystemTime};

use anyhow::anyhow;

pub struct TimestampCheck {
    timestamp_error_tolerance: Duration,
}

use tap_core::receipt::{
    checks::{Check, CheckError, CheckResult},
    WithValueAndTimestamp,
};

use crate::tap::{CheckingReceipt, TapReceipt};

impl TimestampCheck {
    pub fn new(timestamp_error_tolerance: Duration) -> Self {
        Self {
            timestamp_error_tolerance,
        }
    }
}

#[async_trait::async_trait]
impl Check<TapReceipt> for TimestampCheck {
    async fn check(
        &self,
        _: &tap_core::receipt::Context,
        receipt: &CheckingReceipt,
    ) -> CheckResult {
        let timestamp_now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| CheckError::Failed(e.into()))?;
        let min_timestamp = timestamp_now - self.timestamp_error_tolerance;
        let max_timestamp = timestamp_now + self.timestamp_error_tolerance;

        let receipt_timestamp = Duration::from_nanos(receipt.signed_receipt().timestamp_ns());

        if receipt_timestamp < max_timestamp && receipt_timestamp > min_timestamp {
            Ok(())
        } else {
            Err(CheckError::Failed(anyhow!(
                "Receipt timestamp `{}` is outside of current system time +/- timestamp_error_tolerance",
                receipt_timestamp.as_secs()
            )))
        }
    }
}
#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use tap_core::receipt::{checks::Check, Context};
    use test_assets::create_signed_receipt_v2;

    use super::TimestampCheck;
    use crate::tap::{CheckingReceipt, TapReceipt};

    async fn create_signed_receipt_with_custom_timestamp(timestamp_ns: u64) -> CheckingReceipt {
        let receipt = create_signed_receipt_v2()
            .timestamp_ns(timestamp_ns)
            .call()
            .await;
        CheckingReceipt::new(TapReceipt::V2(receipt))
    }

    #[tokio::test]
    async fn test_timestamp_inside_tolerance() {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos()
            + Duration::from_secs(15).as_nanos();
        let timestamp_ns = timestamp as u64;
        let signed_receipt = create_signed_receipt_with_custom_timestamp(timestamp_ns).await;
        let timestamp_check = TimestampCheck::new(Duration::from_secs(30));
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_timestamp_less_than_tolerance() {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos()
            + Duration::from_secs(33).as_nanos();
        let timestamp_ns = timestamp as u64;
        let signed_receipt = create_signed_receipt_with_custom_timestamp(timestamp_ns).await;
        let timestamp_check = TimestampCheck::new(Duration::from_secs(30));
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_timestamp_more_than_tolerance() {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_nanos()
            - Duration::from_secs(33).as_nanos();
        let timestamp_ns = timestamp as u64;
        let signed_receipt = create_signed_receipt_with_custom_timestamp(timestamp_ns).await;
        let timestamp_check = TimestampCheck::new(Duration::from_secs(30));
        assert!(timestamp_check
            .check(&Context::new(), &signed_receipt)
            .await
            .is_err());
    }
}
