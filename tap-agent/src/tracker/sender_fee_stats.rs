// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::VecDeque,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{agent::unaggregated_receipts::UnaggregatedReceipts, backoff::BackoffInfo};

use super::{AllocationStats, DefaultFromExtra, DurationInfo};

#[derive(Debug, Clone, Default)]
pub struct SenderFeeStats {
    pub(super) total_fee: u128,
    pub(super) count: u64,
    // there are some allocations that we don't want it to be
    // heaviest allocation, because they are already marked for finalization,
    // and thus requesting RAVs on their own in their `post_stop` routine.
    pub(super) blocked: bool,
    pub(super) requesting: u128,

    // Buffer info
    pub(super) buffer_info: BufferInfo,

    // Backoff info
    pub(super) backoff_info: BackoffInfo,
}

impl SenderFeeStats {
    pub(super) fn ravable_count(&mut self) -> u64 {
        let allocation_counter = self.count;
        let counter_in_buffer = self.buffer_info.get_count();
        allocation_counter - counter_in_buffer
    }
}

#[derive(Debug, Clone, Default)]
pub struct BufferInfo {
    pub entries: VecDeque<(SystemTime, u128)>,
    pub fee_in_buffer: u128,
    pub duration: Duration,
}

impl BufferInfo {
    pub(super) fn new_entry(&mut self, value: u128, timestamp_ns: u64) {
        let duration_since_epoch = Duration::from_nanos(timestamp_ns);
        // Create a SystemTime from the UNIX_EPOCH plus the duration
        let system_time = UNIX_EPOCH + duration_since_epoch;
        let system_time = system_time
            .checked_add(self.duration)
            .expect("Should be within bounds");
        self.entries.push_back((system_time, value));
        self.fee_in_buffer += value;
    }

    pub(super) fn get_sum(&mut self) -> u128 {
        self.cleanup();
        self.fee_in_buffer
    }

    pub(super) fn get_count(&mut self) -> u64 {
        self.cleanup();
        self.entries.len() as u64
    }

    // O(Receipts expired)
    fn cleanup(&mut self) -> (u128, u64) {
        let now = SystemTime::now();

        let mut total_value_expired = 0;
        let mut total_count_expired = 0;
        while let Some(&(timestamp, value)) = self.entries.front() {
            if now >= timestamp {
                self.entries.pop_front();
                total_value_expired += value;
                total_count_expired += 1;
            } else {
                break;
            }
        }
        self.fee_in_buffer -= total_value_expired;
        (total_value_expired, total_count_expired)
    }
}

impl DefaultFromExtra<DurationInfo> for SenderFeeStats {
    fn default_from_extra(extra: &DurationInfo) -> Self {
        SenderFeeStats {
            buffer_info: BufferInfo {
                duration: extra.buffer_duration,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

impl AllocationStats<UnaggregatedReceipts> for SenderFeeStats {
    fn update(&mut self, v: UnaggregatedReceipts) {
        self.total_fee = v.value;
        self.count = v.counter;
    }

    fn is_allowed_to_trigger_rav_request(&self) -> bool {
        !self.backoff_info.in_backoff() && !self.blocked && self.requesting == 0
    }

    fn get_stats(&self) -> UnaggregatedReceipts {
        UnaggregatedReceipts {
            value: self.total_fee,
            counter: self.count,

            // TODO remove use of UnaggregatedReceipts
            last_id: 0,
        }
    }

    fn get_valid_fee(&mut self) -> u128 {
        self.total_fee - self.buffer_info.get_sum().min(self.total_fee)
    }

    fn get_total_fee(&self) -> u128 {
        self.total_fee
    }
}
