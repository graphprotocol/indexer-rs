use std::sync::Arc;

use axum::{
    body::Bytes,
    http::{self, HeaderName, Request, StatusCode},
    response::IntoResponse,
    Json,
};
use tracing::info;

use crate::{
    query_processor::{FreeQuery, SubgraphDeploymentID},
    server::ServerOptions,
};

pub async fn subgraph_queries(
    req: Request<axum::body::Body>,
    server: axum::extract::Extension<ServerOptions>,
    id: axum::extract::Path<String>,
    query: Bytes,
) -> impl IntoResponse {
    info!("got request");
    let query_string = String::from_utf8_lossy(&query);
    // Initialize id into a subgraph deployment ID
    let subgraph_deployment_id = SubgraphDeploymentID::new(Arc::new(id).to_string());

    // Extract scalar receipt from header and free query auth token for paid or free query
    let _receipt = if let Some(recipt) = req.headers().get("scalar-receipt") {
        match recipt.to_str() {
            Ok(r) => Some(r),
            Err(_) => {
                let error_body = "Bad scalar receipt for subgraph query".to_string();
                return (StatusCode::BAD_REQUEST, Json(error_body)).into_response();
            }
        }
    } else {
        None
    };

    // Extract free query auth token
    let auth_token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|t| t.to_str().ok());
    let mut free = false;
    // determine if the query is paid or authenticated to be free
    if auth_token.is_some()
        && server.free_query_auth_token.is_some()
        && auth_token.unwrap() == server.free_query_auth_token.as_deref().unwrap()
    {
        free = true;
    }

    if free {
        let free_query = FreeQuery {
            subgraph_deployment_id,
            query: query_string.to_string(),
        };
        let res = server
            .query_processor
            .execute_free_query(free_query)
            .await
            .expect("Failed to execute free query");

        info!("Free query executed");
        // take response and send back as json
        match res.status {
            200 => {
                let response_body = res.result.graphql_response;
                let attestable = res.result.attestable.to_string();
                (
                    StatusCode::OK,
                    axum::response::AppendHeaders([
                        (axum::http::header::CONTENT_TYPE, "application/json"),
                        (HeaderName::from_static("graph-attestable"), &attestable),
                    ]),
                    Json(response_body),
                )
                    .into_response()
            }
            _ => {
                let error_body = "Bad subgraph query".to_string();
                (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
            }
        }
    } else {
        let error_body =
            "Query request header missing scalar-receipts or matching auth token".to_string();
        (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
    }
}
