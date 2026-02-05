// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

pub mod cost_model;

use sqlx::{postgres::PgPoolOptions, PgPool};

use crate::constants::{DATABASE_MAX_CONNECTIONS, DATABASE_TIMEOUT};

pub async fn connect(url: &str) -> PgPool {
    tracing::debug!("Connecting to database");

    PgPoolOptions::new()
        .max_connections(DATABASE_MAX_CONNECTIONS)
        .acquire_timeout(DATABASE_TIMEOUT)
        .connect(url)
        .await
        .expect("Should be able to connect to the database")
}
