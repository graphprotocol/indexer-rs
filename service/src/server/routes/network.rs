use axum::{
    body::{Body, Bytes},
    extract::{Extension},
    routing::post,
    http::{Request, Response, StatusCode},
    response::IntoResponse,
    Json, Router,
};

use crate::server::ServerOptions;

pub async fn network_queries(
    Extension(server): Extension<ServerOptions>,
    query: Bytes,
) -> impl IntoResponse {
    println!("routing");
    let query_string = String::from_utf8_lossy(&query);

    // make query to network subgraph - should have endpoint as an input from the environmental variable but just use as a constant here for now
    let request = server
        .query_processor
        .execute_network_free_query(query_string.into_owned())
        .await
        .expect("Failed to execute free network subgraph query");

    // take response and send back as json
    match request.status {
        200 => {
            let response_body = request.result.graphQLResponse;
            (StatusCode::OK, Json(response_body))
        }
        _ => {
            let error_body = "Bad subgraph query".to_string();
            (StatusCode::BAD_REQUEST, Json(error_body))
        }
    }
}
