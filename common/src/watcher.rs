// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0


//! This is a module that reimplements eventuals using
//! tokio::watch module and fixing some problems that eventuals
//! usually carry like initializing things without initializing
//! its values

use std::{future::Future, time::Duration};

use tokio::{
    select,
    sync::watch,
    time::{self, sleep},
};
use tracing::warn;

/// Creates a new watcher that auto initializes it with initial_value
/// and updates it given an interval
pub async fn new_watcher<T, F, Fut>(
    interval: Duration,
    function: F,
) -> anyhow::Result<watch::Receiver<T>>
where
    F: Fn() -> Fut + Send + 'static,
    T: Sync + Send + 'static,
    Fut: Future<Output = anyhow::Result<T>> + Send,
{
    let initial_value = function().await?;

    let (tx, rx) = watch::channel(initial_value);

    tokio::spawn(async move {
        // Refresh indexer allocations every now and then
        let mut time_interval = time::interval(interval);
        time_interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            time_interval.tick().await;
            let result = function().await;
            match result {
                Ok(allocations) => tx.send(allocations).expect("Failed to update channel"),
                Err(err) => {
                    // TODO mark it as delayed
                    warn!(error = %err, "There was an error while updating watcher");
                    // Sleep for a bit before we retry
                    sleep(interval.div_f32(2.0)).await;
                }
            }
        }
    });
    Ok(rx)
}


/// Join two watch::Receiver
pub fn join_watcher<T1, T2>(
    mut receiver_1: watch::Receiver<T1>,
    mut receiver_2: watch::Receiver<T2>,
) -> watch::Receiver<(T1, T2)>
where
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    let (tx, rx) = watch::channel((receiver_1.borrow().clone(), receiver_2.borrow().clone()));

    tokio::spawn(async move {
        loop {
            select! {
                Ok(())= receiver_1.changed() =>{},
                Ok(())= receiver_2.changed() =>{},
                else=>{
                    // Something is wrong.
                    panic!("receiver_1 or receiver_2 was dropped");
                }
            }
            tx.send((receiver_1.borrow().clone(), receiver_2.borrow().clone()))
                .expect("Failed to update signers channel");
        }
    });
    rx
}

/// Maps a watch::Receiver into another type
pub fn map_watcher<T1, T2, F>(
    mut receiver: watch::Receiver<T1>,
    map_function: F,
) -> watch::Receiver<T2>
where
    F: Fn(T1) -> T2 + Send + 'static,
    T1: Default + Clone + Sync + Send + 'static,
    T2: Sync + Send + 'static,
{
    let initial_value = map_function(receiver.borrow().clone());
    let (tx, rx) = watch::channel(initial_value);

    tokio::spawn(async move {
        loop {
            match receiver.changed().await {
                Ok(_) => {}
                Err(_) => panic!("reciever was dropped"),
            }
            let current_val = receiver.borrow().clone();
            let mapped_value = map_function(current_val);
            tx.send(mapped_value).expect("Failed to update channel");
        }
    });
    rx
}
