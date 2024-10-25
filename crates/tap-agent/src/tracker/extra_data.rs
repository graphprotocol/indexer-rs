// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

pub trait DefaultFromExtra<E> {
    fn default_from_extra(extra: &E) -> Self;
}

#[derive(Debug, Clone)]
pub struct DurationInfo {
    pub(super) buffer_duration: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct NoExtraData;

impl<T> DefaultFromExtra<NoExtraData> for T
where
    T: Default,
{
    fn default_from_extra(_: &NoExtraData) -> Self {
        Default::default()
    }
}
