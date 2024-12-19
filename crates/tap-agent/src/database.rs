// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use sqlx::{postgres::PgPoolOptions, PgPool};

pub async fn connect(config: indexer_config::DatabaseConfig) -> PgPool {
    let url = &config.get_formated_postgres_url();
    tracing::debug!(
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
