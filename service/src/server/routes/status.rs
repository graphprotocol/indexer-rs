use axum::{
    http::{Request, StatusCode},
    response::IntoResponse,
    Extension, Json,
};

use reqwest::{header, Client};

use crate::server::ServerOptions;

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

    let response: reqwest::Response = request
        .send()
        .await
        .map_err(|e| {
            let error_body = format!("Bad request: {}", e);
            (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
        })
        .unwrap();

    response
        .text()
        .await
        .expect("Failed to read response as string")
}
