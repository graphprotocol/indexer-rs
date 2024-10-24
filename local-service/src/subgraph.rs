use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use crate::{allocations::Allocations, keys::Indexer};

pub(crate) const NETWORK_SUBGRAPH_ROUTE: &str = "/network_subgraph";

pub(crate) async fn network_subgraph(indexer: Indexer) -> impl IntoResponse {
    Json(GraphqlResponse::Allocations(Allocations::new(indexer)))
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum GraphqlResponse {
    Allocations(Allocations),
}
