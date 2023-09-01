// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use sqlx::PgPool;

use reqwest::Client;

pub mod resolver;
pub mod schema;

// Unified query object for resolvers
#[derive(Default)]
pub struct QueryRoot;

/// Indexer management client
#[derive(Debug, Clone)]
pub struct IndexerManagementClient {
    database: PgPool, // Only referenced
    client: Client,   // it is Arc
}

impl IndexerManagementClient {
    pub async fn new(database: PgPool) -> IndexerManagementClient {
        let client = reqwest::Client::builder()
            .user_agent("indexer-service")
            .build()
            .expect("Could not build a HTTP client");
        IndexerManagementClient { database, client }
    }

    pub fn database(&self) -> &PgPool {
        &self.database
    }
}
