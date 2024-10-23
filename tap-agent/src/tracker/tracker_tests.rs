use crate::{agent::unaggregated_receipts::UnaggregatedReceipts, tracker::SenderFeeTracker};

use super::SimpleFeeTracker;
use alloy::primitives::address;
use std::{
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[test]
fn test_allocation_id_tracker() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    let mut tracker = SimpleFeeTracker::default();
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.update(allocation_id_0, 10);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_2, 20);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.update(allocation_id_1, 30);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 60);

    tracker.update(allocation_id_2, 10);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 50);

    tracker.update(allocation_id_2, 40);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 80);

    tracker.update(allocation_id_1, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 50);

    tracker.update(allocation_id_2, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_0, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);
}

impl From<u128> for UnaggregatedReceipts {
    fn from(value: u128) -> Self {
        UnaggregatedReceipts {
            value,
            last_id: 0,
            counter: 0,
        }
    }
}

#[test]
fn test_blocking_allocations() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    const BUFFER_WINDOW: Duration = Duration::from_millis(0);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.update(allocation_id_0, 10.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.block_allocation_id(allocation_id_0);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.unblock_allocation_id(allocation_id_0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));

    tracker.update(allocation_id_2, 20.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.block_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.unblock_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

    tracker.update(allocation_id_1, 30.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 60);

    tracker.update(allocation_id_2, 10.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 50);

    tracker.update(allocation_id_2, 40.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 80);

    tracker.update(allocation_id_1, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 50);

    tracker.update(allocation_id_2, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_0, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);
}

fn get_current_timestamp_u64_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[test]
fn test_buffer_tracker_window() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    const BUFFER_WINDOW: Duration = Duration::from_millis(20);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.add(allocation_id_0, 10, get_current_timestamp_u64_ns());
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(tracker.get_total_fee(), 10);

    sleep(BUFFER_WINDOW);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_ravable_total_fee(), 10);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.add(allocation_id_2, 20, get_current_timestamp_u64_ns());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_ravable_total_fee(), 10);
    assert_eq!(tracker.get_total_fee(), 30);

    sleep(BUFFER_WINDOW);

    tracker.block_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_ravable_total_fee(), 30);
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.unblock_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

    tracker.add(allocation_id_1, 30, get_current_timestamp_u64_ns());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_ravable_total_fee(), 30);
    assert_eq!(tracker.get_total_fee(), 60);

    sleep(BUFFER_WINDOW);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_ravable_total_fee(), 60);
    assert_eq!(tracker.get_total_fee(), 60);

    tracker.add(allocation_id_2, 20, get_current_timestamp_u64_ns());
    // we just removed, the buffer should have been removed as well
    tracker.update(allocation_id_2, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_ravable_total_fee(), 20);
    assert_eq!(tracker.get_total_fee(), 40);

    sleep(BUFFER_WINDOW);

    tracker.add(allocation_id_2, 200, get_current_timestamp_u64_ns());
    tracker.update(allocation_id_2, 100.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(tracker.get_total_fee(), 140);

    sleep(BUFFER_WINDOW);

    tracker.update(allocation_id_2, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_ravable_total_fee(), 40);
    assert_eq!(tracker.get_total_fee(), 40);

    tracker.update(allocation_id_1, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_ravable_total_fee(), 10);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_0, 0.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(tracker.get_total_fee(), 0);
}

#[test]
fn test_filtered_backed_off_allocations() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    const BACK_SLEEP_DURATION: Duration = Duration::from_millis(201);

    const BUFFER_WINDOW: Duration = Duration::from_millis(0);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.update(allocation_id_0, 10.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_1, 20.into());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 30);

    // Simulate failed rav and backoff
    tracker.failed_rav_backoff(allocation_id_1);

    // Heaviest should be the first since its not blocked nor failed
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 30);

    sleep(BACK_SLEEP_DURATION);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 30);
}

#[test]
fn test_ongoing_rav_requests() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    const BUFFER_WINDOW: Duration = Duration::from_millis(0);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);

    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.add(allocation_id_0, 10, get_current_timestamp_u64_ns());
    tracker.add(allocation_id_1, 20, get_current_timestamp_u64_ns());
    tracker.add(allocation_id_2, 30, get_current_timestamp_u64_ns());
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 60);
    assert_eq!(tracker.get_ravable_total_fee(), 60);

    tracker.start_rav_request(allocation_id_2);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee(), 30);
    assert_eq!(tracker.get_ravable_total_fee(), 30);

    tracker.finish_rav_request(allocation_id_2);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 60);
    assert_eq!(tracker.get_ravable_total_fee(), 60);
}

#[test]
fn check_counter_and_fee_outside_buffer_unordered() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    const BUFFER_WINDOW: Duration = Duration::from_millis(20);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);

    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(
        tracker.get_count_outside_buffer_for_allocation(&allocation_id_0),
        0
    );

    tracker.add(allocation_id_0, 10, get_current_timestamp_u64_ns());
    assert_eq!(tracker.get_ravable_total_fee(), 0);
    assert_eq!(
        tracker.get_count_outside_buffer_for_allocation(&allocation_id_0),
        0
    );

    sleep(BUFFER_WINDOW);

    assert_eq!(tracker.get_ravable_total_fee(), 10);
    assert_eq!(
        tracker.get_count_outside_buffer_for_allocation(&allocation_id_0),
        1
    );

    tracker.add(allocation_id_2, 20, get_current_timestamp_u64_ns());
    assert_eq!(
        tracker.get_count_outside_buffer_for_allocation(&allocation_id_2),
        0
    );
    assert_eq!(tracker.get_ravable_total_fee(), 10);

    sleep(BUFFER_WINDOW);

    tracker.block_allocation_id(allocation_id_2);
    assert_eq!(
        tracker.get_count_outside_buffer_for_allocation(&allocation_id_2),
        1
    );
    assert_eq!(tracker.get_ravable_total_fee(), 30);
}

#[test]
fn check_get_count_updates_sum() {
    let allocation_id_0 = address!("abababababababababababababababababababab");

    const BUFFER_WINDOW: Duration = Duration::from_millis(20);
    let mut tracker = SenderFeeTracker::new(BUFFER_WINDOW);

    tracker.add(allocation_id_0, 10, get_current_timestamp_u64_ns());
    let expiring_sum = tracker
        .id_to_fee
        .get_mut(&allocation_id_0)
        .expect("there should be something here");
    assert_eq!(expiring_sum.buffer_info.get_sum(), 10);
    assert_eq!(expiring_sum.buffer_info.get_count(), 1);

    sleep(BUFFER_WINDOW);

    assert_eq!(expiring_sum.buffer_info.get_sum(), 0);
    assert_eq!(expiring_sum.buffer_info.get_count(), 0);

    tracker.add(allocation_id_0, 10, get_current_timestamp_u64_ns());
    let expiring_sum = tracker
        .id_to_fee
        .get_mut(&allocation_id_0)
        .expect("there should be something here");

    assert_eq!(expiring_sum.buffer_info.get_count(), 1);
    assert_eq!(expiring_sum.buffer_info.get_sum(), 10);

    sleep(BUFFER_WINDOW);

    assert_eq!(expiring_sum.buffer_info.get_count(), 0);
    assert_eq!(expiring_sum.buffer_info.get_sum(), 0);
}
