use axum::{
    body::Bytes,
    http::{Request, StatusCode},
    response::IntoResponse,
    Json,
};
use reqwest::Request as MiddlewareRequest;
use reqwest::{header, Client};
use tracing::info;

use crate::server::ServerOptions;

// Custom middleware function to process the request before reaching the main handler
pub async fn status_middleware(
    server: axum::extract::Extension<ServerOptions>,
    req: Request::<axum::body::Bytes>,
    // query: Bytes,
    // RawQuery(query): RawQuery,
) -> impl IntoResponse {
    info!("received a request to status middleware: {:#?}", &req);
    // Extract the incoming GraphQL operation
    let req_hd = req.headers();
    let req_hd_3 = req.headers();
    let req_body = req.body().clone();
    let req_body_2 = req.into_body();
    // let query: Bytes = hyper::body::to_bytes(req_body_2)
    //     .await
    //     .map_err(|e| (StatusCode::BAD_REQUEST, Json(e)))
    //     .unwrap();
    let query_string = String::from_utf8_lossy(&req_body_2);
    
    info!("need to filter graphQL root fields: {:#?}", &req_body);
    // let mut graphql_request = async_graphql::http::parse_query_string(&query_string)
    //     .map_err(|e| {
    //         let error_body = format!("Bad request format: {}", e);
    //         (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
    //     })
    //     .unwrap();

    // Limit GraphQL API interface to the Graph node status endpoint
    // Filtering the index-node server schema to the queries we want to expose externally
    // indexingStatuses, apiVersions - needed by gateways, and explorer
    // others are used for debugging data discrepancies
    let _supported_root_fields = [
        "indexingStatuses",
        "publicProofsOfIndexing",
        "entityChangesInBlock",
        "blockData",
        "cachedEthereumCalls",
        "subgraphFeatures",
        "apiVersions",
    ];

    // For the ease of passing through the query, do not filter for individual operations.
    // If there is an unsupported root field, reject to serve the request altogether.

    // let _parsed_query = graphql_request
    //     .parsed_query()
    //     .map_err(|e| {
    //         let error_body = format!("Unparseable query format: {}", e);
    //         (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
    //     })
    //     .unwrap();

    // // Use `filter` and `collect` instead of `find` to filters the operations instead
    // // This currently just fail any request with unsupported fields but might be limiting
    // if parsed_query.operations.iter().find(|&(Some(&name), _)|{
    //     supported_root_fields.contains(&name.as_str())
    // }).is_some()
    // {
    //     let error_body = format!("Query contains unsupported fields");
    //     return (StatusCode::BAD_REQUEST, Json(error_body)).into_response();
    // };

    // Pass the modified operation to the actual endpoint
    // let (method, url, mut headers, body, timeout, version) = req.pieces();
    let request = Client::new()
        .post(&server.graph_node_status_endpoint)
        .body(req_body_2)
        .header(header::CONTENT_TYPE, "application/json");

    let response: reqwest::Response = request
        .send()
        .await
        .map_err(|e| {
            let error_body = format!("Bad request format: {}", e);
            (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
        })
        .unwrap();

    // let response = async_graphql::response::Response(response);

    // match response. {
    //         200 => {
    //             let response_body = res.result.graphql_response;
    //             let attestable = res.result.attestable.to_string();
    //             (
    //                 StatusCode::OK,
    //                 axum::response::AppendHeaders([
    //                     (axum::http::header::CONTENT_TYPE, "application/json"),
    //                     (HeaderName::from_static("graph-attestable"), &attestable),
    //                 ]),
    //                 Json(response_body),
    //             )
    //                 .into_response()
    //         }
    //         _ => {
    //             let error_body = "Bad indexing status query".to_string();
    //             (StatusCode::BAD_REQUEST, Json(error_body)).into_response()
    //         }
    //     }

    // // actually parse the JSON for the graphQL schema
    // let response_text = response.text().await?;
    // Ok(UnattestedQueryResult {
    //     graphql_response: response_text,
    //     attestable: false,
    // })
    // let response = async_graphql::http::respond(service.execute(operation).await);

    response
        .text()
        .await
        .expect("Failed to read response as string")
}
