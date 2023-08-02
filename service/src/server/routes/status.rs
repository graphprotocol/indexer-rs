use axum::{
    http::{Request, StatusCode},
    response::IntoResponse,
    Extension, Json,
};

use reqwest::{header, Client};

use crate::server::ServerOptions;

use super::bad_request_response;

// Custom middleware function to process the request before reaching the main handler
pub async fn status_queries(
    Extension(server): Extension<ServerOptions>,
    req: Request<axum::body::Body>,
) -> impl IntoResponse {
    let req_body = req.into_body();
    // TODO: Extract the incoming GraphQL operation and filter root fields
    // Pass the modified operation to the actual endpoint

    let request = Client::new()
        .post(&server.graph_node_status_endpoint)
        .body(req_body)
        .header(header::CONTENT_TYPE, "application/json");

    let response: reqwest::Response = match request.send().await {
        Ok(r) => r,
        Err(e) => return bad_request_response(&e.to_string()),
    };

    match response.text().await {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        _ => bad_request_response("Response from Graph node cannot be parsed as a string"),
    }
}
