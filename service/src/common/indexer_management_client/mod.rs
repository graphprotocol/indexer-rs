// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use sqlx::PgPool;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod resolver;
pub mod schema;

/// Indexer management client
///
/// This is Arc internally, so it can be cloned and shared between threads.
#[derive(Debug, Clone)]
pub struct IndexerManagementClient {
    database: PgPool,
    client: Client, // it is Arc
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphQLQuery {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
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
