// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use sqlx::{postgres::PgPoolOptions, PgPool};

use std::time::Duration;
use tracing::debug;

use crate::config;

pub async fn connect(config: &config::Postgres) -> PgPool {
    let url = format!(
        "postgresql://{}:{}@{}:{}/{}",
        config.postgres_username,
        config.postgres_password,
        config.postgres_host,
        config.postgres_port,
        config.postgres_database
    );

    std::env::set_var("DATABASE_URL", &url);

    debug!("Connecting to database");

    PgPoolOptions::new()
        .max_connections(50)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&url)
        .await
        .expect("Could not connect to DATABASE_URL")
}
