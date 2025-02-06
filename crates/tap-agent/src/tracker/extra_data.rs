// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

/// Default values for a given extra data E
pub trait DefaultFromExtra<E> {
    /// Generates a new default Self given some extra data
    fn default_from_extra(extra: &E) -> Self;
}

/// Buffer information
#[derive(Debug, Clone)]
pub struct DurationInfo {
    pub(super) buffer_duration: Duration,
}

/// No Extra Data struct
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
