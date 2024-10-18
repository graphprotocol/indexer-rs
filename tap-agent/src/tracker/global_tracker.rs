use std::ops::{AddAssign, SubAssign};

use crate::agent::unaggregated_receipts::UnaggregatedReceipts;

use super::GlobalFeeTracker;

pub trait GlobalTracker<T>: SubAssign<T> + AddAssign<T> + Sized {
    fn get_total_fee(&self) -> u128;
}

impl GlobalTracker<u128> for u128 {
    fn get_total_fee(&self) -> u128 {
        *self
    }
}

impl GlobalTracker<UnaggregatedReceipts> for GlobalFeeTracker {
    fn get_total_fee(&self) -> u128 {
        self.total_fee - self.requesting
    }
}
