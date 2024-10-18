use super::{SenderFeeStats, SimpleFeeTracker};
use alloy::primitives::address;
use std::{thread::sleep, time::Duration};

#[test]
fn test_allocation_id_tracker() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    let mut tracker = SimpleFeeTracker::<SenderFeeStats>::default();
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.update(allocation_id_0, 10);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.block_allocation_id(allocation_id_0);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.unblock_allocation_id(allocation_id_0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));

    tracker.update(allocation_id_2, 20);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.block_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.unblock_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

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

#[test]
fn test_buffer_tracker_window() {
    let allocation_id_0 = address!("abababababababababababababababababababab");
    let allocation_id_1 = address!("bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc");
    let allocation_id_2 = address!("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");

    const BUFFER_WINDOW: Duration = Duration::from_millis(20);
    let mut tracker = SimpleFeeTracker::new(BUFFER_WINDOW);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
    assert_eq!(tracker.get_total_fee(), 0);

    tracker.add(allocation_id_0, 10);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
    assert_eq!(tracker.get_total_fee(), 10);

    sleep(BUFFER_WINDOW);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.add(allocation_id_2, 20);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
    assert_eq!(tracker.get_total_fee(), 30);

    sleep(BUFFER_WINDOW);

    tracker.block_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 30);
    assert_eq!(tracker.get_total_fee(), 30);

    tracker.unblock_allocation_id(allocation_id_2);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));

    tracker.add(allocation_id_1, 30);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_2));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 30);
    assert_eq!(tracker.get_total_fee(), 60);

    sleep(BUFFER_WINDOW);

    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 60);
    assert_eq!(tracker.get_total_fee(), 60);

    tracker.add(allocation_id_2, 20);
    // we just removed, the buffer should have been removed as well
    tracker.update(allocation_id_2, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 40);
    assert_eq!(tracker.get_total_fee(), 40);

    sleep(BUFFER_WINDOW);

    tracker.add(allocation_id_2, 200);
    tracker.update(allocation_id_2, 100);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
    assert_eq!(tracker.get_total_fee(), 140);

    sleep(BUFFER_WINDOW);

    tracker.update(allocation_id_2, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_1));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 40);
    assert_eq!(tracker.get_total_fee(), 40);

    tracker.update(allocation_id_1, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), Some(allocation_id_0));
    assert_eq!(tracker.get_total_fee_outside_buffer(), 10);
    assert_eq!(tracker.get_total_fee(), 10);

    tracker.update(allocation_id_0, 0);
    assert_eq!(tracker.get_heaviest_allocation_id(), None);
    assert_eq!(tracker.get_total_fee_outside_buffer(), 0);
    assert_eq!(tracker.get_total_fee(), 0);
}
