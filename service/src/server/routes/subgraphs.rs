use std::sync::Arc;

use axum::{response::IntoResponse, Json, http::StatusCode, body::Bytes};
use tracing::info;

use crate::{server::ServerOptions, query_processor::{SubgraphDeploymentID, FreeQuery}};

pub async fn subgraph_queries(
    server: axum::extract::Extension<ServerOptions>,
    id: axum::extract::Path<String>,
    query: Bytes,
) -> impl IntoResponse {
    info!("got request");
    let query_string = String::from_utf8_lossy(&query);

    // Initialize id into a subgraph deployment ID and make a freeQuery to graph node
    let subgraph_deployment_id = SubgraphDeploymentID::new(Arc::new(id).to_string());
    let free_query = FreeQuery {
        subgraph_deployment_id,
        query: query_string.to_string(),
    };
    info!("made free query");
    let res = server
        .query_processor
        .execute_free_query(free_query)
        .await
        .expect("Failed to execute free query");

    info!("query executed");
    // take response and send back as json
    match res.status {
        200 => {
            let response_body = res.result.graphQLResponse;
            (StatusCode::OK, Json(response_body))
        }
        _ => {
            let error_body = "Bad subgraph query".to_string();
            (StatusCode::BAD_REQUEST, Json(error_body))
        }
    }
}
