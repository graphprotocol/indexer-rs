// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

pub mod cost_model;
pub mod dips;

use std::time::Duration;

use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::debug;

pub async fn connect(url: &str) -> PgPool {
    debug!("Connecting to database");

    PgPoolOptions::new()
        .max_connections(50)
        .acquire_timeout(Duration::from_secs(3))
        .connect(url)
        .await
        .expect("Should be able to connect to the database")
}
