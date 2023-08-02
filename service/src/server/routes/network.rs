use axum::{
    extract::Extension,
    http::{self, Request, StatusCode},
    response::IntoResponse,
    Json,
};
use hyper::http::HeaderName;

use crate::server::ServerOptions;

use super::{bad_request_response, response_body_to_query_string};

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
        return bad_request_response("Not enabled or authorized query");
    }

    // Serve query using query processor
    let req_body = req.into_body();
    let query_string = match response_body_to_query_string(req_body).await {
        Ok(q) => q,
        Err(e) => return bad_request_response(&e.to_string()),
    };

    let request = server
        .query_processor
        .execute_network_free_query(query_string)
        .await
        .expect("Failed to execute free network subgraph query");

    match request.status {
        200 => {
            let response_body = request.result.graphql_response;
            (
                StatusCode::OK,
                axum::response::AppendHeaders([(
                    HeaderName::from_static("graph-attestable"),
                    "false",
                )]),
                Json(response_body),
            )
                .into_response()
        }
        _ => bad_request_response("Bad response from Graph node"),
    }
}
