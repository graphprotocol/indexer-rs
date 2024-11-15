use indexer_config::GraphNodeConfig;
use sqlx::Postgres;

#[derive(Clone)]
pub struct SubgraphServiceState {
    pub database: sqlx::Pool<Postgres>,

    pub graph_node_client: reqwest::Client,
    pub graph_node_config: &'static GraphNodeConfig,
}
