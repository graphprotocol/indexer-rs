use axum::{
    body::Bytes,
    extract::Extension,
    http::{self, Request, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::server::ServerOptions;

pub async fn network_queries(
    req: Request<axum::body::Body>,
    Extension(server): Extension<ServerOptions>,
    query: Bytes,
) -> impl IntoResponse {
    // Extract free query auth token
    let auth_token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|t| t.to_str().ok());

    // Serve only if enabled by indexer and request auth token matches
    if !(server.serve_network_subgraph
        && auth_token.is_some()
        && server.network_subgraph_auth_token.is_some()
        && auth_token.unwrap() == server.network_subgraph_auth_token.as_deref().unwrap())
    {
        let error_body = "Bad network subgraph query".to_string();
        return (StatusCode::BAD_REQUEST, Json(error_body)).into_response();
    }

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
            let response_body = request.result.graphql_response;
            (
                StatusCode::OK,
                axum::response::AppendHeaders([(
                    axum::http::header::CONTENT_TYPE,
                    "application/json",
                )]),
                Json(response_body),
            )
                .into_response()
        }
        _ => {
            let error_body = "Bad network subgraph query".to_string();
            (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
        }
    }
}
