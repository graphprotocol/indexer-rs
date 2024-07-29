// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::debug;

use crate::config;

pub async fn connect(config: &config::Postgres) -> PgPool {
    let url = &config.postgres_url;
    debug!(
        postgres_host = tracing::field::debug(&url.host()),
        postgres_port = tracing::field::debug(&url.port()),
        postgres_database = tracing::field::debug(&url.path()),
        "Connecting to database"
    );
    PgPoolOptions::new()
        .max_connections(50)
        .acquire_timeout(Duration::from_secs(3))
        .connect(url.as_str())
        .await
        .expect("Could not connect to DATABASE_URL")
}
