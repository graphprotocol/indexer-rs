use std::{
    collections::VecDeque,
    ops::AddAssign,
    time::{Duration, Instant},
};
use tracing::error;

use super::{AllocationStats, DefaultFromExtra, DurationInfo};

#[derive(Debug, Clone, Default)]
pub struct SenderFeeStats {
    pub(super) total_fee: u128,
    // there are some allocations that we don't want it to be
    // heaviest allocation, because they are already marked for finalization,
    // and thus requesting RAVs on their own in their `post_stop` routine.
    pub(super) blocked: bool,

    // buffer
    pub(super) entries: VecDeque<(Instant, u128)>,
    pub(super) fee_in_buffer: u128,
    pub(super) duration: Duration,
}

impl DefaultFromExtra<DurationInfo> for SenderFeeStats {
    fn default_from_extra(extra: &DurationInfo) -> Self {
        SenderFeeStats {
            duration: extra.buffer_duration,
            ..Default::default()
        }
    }
}

impl SenderFeeStats {
    pub(super) fn get_sum(&mut self) -> u128 {
        let now = Instant::now();
        while let Some(&(timestamp, value)) = self.entries.front() {
            if now.duration_since(timestamp) >= self.duration {
                self.entries.pop_front();
                self.fee_in_buffer -= value;
            } else {
                break;
            }
        }
        self.fee_in_buffer
    }
}

impl AllocationStats<u128> for SenderFeeStats {
    fn update(&mut self, v: u128) {
        self.total_fee = v;
    }

    fn should_filter_out(&self) -> bool {
        self.blocked
    }

    fn get_fee(&self) -> u128 {
        self.total_fee
    }

    fn get_valid_fee(&mut self) -> u128 {
        self.get_fee() - self.get_sum().min(self.total_fee)
    }
}

impl AddAssign<u128> for SenderFeeStats {
    fn add_assign(&mut self, rhs: u128) {
        self.total_fee += rhs;

        self.total_fee = self.total_fee.checked_add(rhs).unwrap_or_else(|| {
            // This should never happen, but if it does, we want to know about it.
            error!(
                "Overflow when adding receipt value {} to total fee {}. \
                    Setting total fee to u128::MAX.",
                self.total_fee, rhs
            );
            u128::MAX
        });
    }
}
