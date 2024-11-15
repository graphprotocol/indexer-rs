use std::time::Duration;

use indexer_config::GraphNodeConfig;
use sqlx::Postgres;

#[derive(Clone)]
pub struct SubgraphServiceState {
    pub database: sqlx::Pool<Postgres>,

    pub graph_node_client: reqwest::Client,
    pub graph_node_config: &'static GraphNodeConfig,
}

impl SubgraphServiceState {
    pub fn new(
        database: sqlx::Pool<Postgres>,
        graph_node_config: &'static GraphNodeConfig,
    ) -> Self {
        Self {
            database,
            graph_node_client: reqwest::ClientBuilder::new()
                .tcp_nodelay(true)
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to init HTTP client for Graph Node"),
            graph_node_config,
        }
    }
}
