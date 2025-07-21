// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Test utilities for database setup using testcontainers
//!
//! This module provides shared database setup functionality for all tests,
//! ensuring consistent test environment without conflicts.

#[cfg(test)]
use std::sync::OnceLock;

#[cfg(test)]
use testcontainers_modules::{postgres, testcontainers::runners::AsyncRunner};

#[cfg(test)]
static TEST_DB_POOL: OnceLock<sqlx::PgPool> = OnceLock::new();

#[cfg(test)]
static TEST_DB_URL: OnceLock<String> = OnceLock::new();

/// Set up a shared test database using testcontainers
///
/// This function ensures that all tests use the same database instance,
/// avoiding conflicts between SQLx's test infrastructure and multiple
/// container setups.
#[cfg(test)]
pub async fn setup_shared_test_db() -> (sqlx::PgPool, String) {
    // Return existing setup if already initialized
    if let (Some(pool), Some(url)) = (TEST_DB_POOL.get(), TEST_DB_URL.get()) {
        return (pool.clone(), url.clone());
    }

    // Start PostgreSQL container
    let pg_container = postgres::Postgres::default()
        .start()
        .await
        .expect("Failed to start PostgreSQL container");

    let host_port = pg_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get container port");

    // Use localhost to match SQLx testing expectations
    let connection_string = format!("postgres://postgres:postgres@localhost:{host_port}/postgres");

    // Create connection pool
    let pool = sqlx::PgPool::connect(&connection_string)
        .await
        .expect("Failed to connect to test database");

    tracing::info!(
        "Shared test PostgreSQL container started: {}",
        connection_string
    );

    // Store globally for reuse
    let _ = TEST_DB_POOL.set(pool.clone());
    let _ = TEST_DB_URL.set(connection_string.clone());

    // Set DATABASE_URL for SQLx macros (only if not already set)
    if std::env::var("DATABASE_URL").is_err() {
        std::env::set_var("DATABASE_URL", &connection_string);
    }

    // Keep container alive by forgetting it (will be cleaned up when process ends)
    std::mem::forget(pg_container);

    (pool, connection_string)
}

/// Get the shared test database pool
///
/// This function returns the already initialized test database pool,
/// or panics if setup_shared_test_db() hasn't been called yet.
#[cfg(test)]
pub fn get_shared_test_db_pool() -> sqlx::PgPool {
    TEST_DB_POOL
        .get()
        .expect("Test database not initialized. Call setup_shared_test_db() first.")
        .clone()
}

/// Get the shared test database URL
#[cfg(test)]
pub fn get_shared_test_db_url() -> String {
    TEST_DB_URL
        .get()
        .expect("Test database not initialized. Call setup_shared_test_db() first.")
        .clone()
}
