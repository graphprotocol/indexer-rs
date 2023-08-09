// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{Context, Object, Schema};
use async_graphql::{EmptyMutation, EmptySubscription};

pub(crate) type ServiceSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

pub(crate) struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn hello(&self, _ctx: &Context<'_>) -> &'static str {
        "blahblahblaaaaaa"
    }
}
