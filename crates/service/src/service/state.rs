use reqwest::Url;
use sqlx::Postgres;

pub struct SubgraphServiceState {
    pub database: sqlx::Pool<Postgres>,

    pub graph_node_client: reqwest::Client,
    pub graph_node_query_base_url: &'static Url,
    pub graph_node_status_url: &'static Url,
}
