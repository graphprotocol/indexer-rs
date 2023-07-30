use axum::{
    body::Bytes,
    extract::Extension,
    http::{self, Request, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::server::ServerOptions;

pub async fn network_queries(
    Extension(server): Extension<ServerOptions>,
    req: Request<axum::body::Body>,
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
        let error_body = "Not enabled or authorized query".to_string();
        return (
            StatusCode::BAD_REQUEST,
            axum::response::AppendHeaders([(axum::http::header::CONTENT_TYPE, "application/json")]),
            Json(error_body),
        );
    }

    // Serve query using query processor
    let req_body = req.into_body();
    let query: Bytes = hyper::body::to_bytes(req_body)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                axum::response::AppendHeaders([(
                    axum::http::header::CONTENT_TYPE,
                    "application/json",
                )]),
                Json(e),
            )
        })
        .unwrap();
    let query_string = String::from_utf8_lossy(&query);

    let request = server
        .query_processor
        .execute_network_free_query(query_string.into_owned())
        .await
        .expect("Failed to execute free network subgraph query");

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
        }
        _ => {
            let error_body = "Bad subgraph query".to_string();
            (
                StatusCode::BAD_REQUEST,
                axum::response::AppendHeaders([(
                    axum::http::header::CONTENT_TYPE,
                    "application/json",
                )]),
                Json(error_body),
            )
        }
    }
}
