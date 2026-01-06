// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, sync::LazyLock, time::Instant};

use axum::{
    body::Bytes,
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
};
use graphql::graphql_parser::query as q;
use serde::Deserialize;

use crate::{error::SubgraphServiceError, service::GraphNodeState};

static SUPPORTED_ROOT_FIELDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "indexingStatuses",
        "chains",
        "latestBlock",
        "earliestBlock",
        "publicProofsOfIndexing",
        "entityChangesInBlock",
        "blockData",
        "blockHashFromNumber",
        "cachedEthereumCalls",
        "subgraphFeatures",
        "apiVersions",
        "version",
    ]
    .into_iter()
    .collect()
});

/// Minimal struct to extract the query string for validation
#[derive(Deserialize)]
struct StatusRequest {
    query: String,
}

/// Forwards GraphQL status queries to graph-node after validating allowed root fields.
///
/// This handler uses direct HTTP forwarding for optimal performance, avoiding
/// the overhead of multiple serialization/deserialization layers.
pub async fn status(
    State(state): State<GraphNodeState>,
    body: Bytes,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    let start = Instant::now();

    // Parse request to extract query for validation
    let request: StatusRequest = serde_json::from_slice(&body)
        .map_err(|e| SubgraphServiceError::InvalidStatusQuery(e.into()))?;

    // Parse and validate GraphQL query fields
    let query: q::Document<String> = q::parse_query(&request.query)
        .map_err(|e| SubgraphServiceError::InvalidStatusQuery(e.into()))?;

    let root_fields = query
        .definitions
        .iter()
        // This gives us all root selection sets
        .filter_map(|def| match def {
            q::Definition::Operation(op) => match op {
                q::OperationDefinition::Query(query) => Some(&query.selection_set),
                q::OperationDefinition::SelectionSet(selection_set) => Some(selection_set),
                _ => None,
            },
            q::Definition::Fragment(fragment) => Some(&fragment.selection_set),
        })
        // This gives us all field names of root selection sets (and potentially non-root fragments)
        .flat_map(|selection_set| {
            selection_set
                .items
                .iter()
                .filter_map(|item| match item {
                    q::Selection::Field(field) => Some(&field.name),
                    _ => None,
                })
                .collect::<HashSet<_>>()
        });

    let unsupported_root_fields: Vec<_> = root_fields
        .filter(|field| !SUPPORTED_ROOT_FIELDS.contains(field.as_str()))
        .map(ToString::to_string)
        .collect();

    if !unsupported_root_fields.is_empty() {
        return Err(SubgraphServiceError::UnsupportedStatusQueryFields(
            unsupported_root_fields,
        ));
    }

    tracing::debug!(
        elapsed_ms = start.elapsed().as_millis(),
        "Status query validated"
    );

    // Forward request to graph-node directly
    let forward_start = Instant::now();
    let response = state
        .graph_node_client
        .post(state.graph_node_status_url.clone())
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| SubgraphServiceError::StatusQueryError(e.into()))?;

    tracing::debug!(
        elapsed_ms = forward_start.elapsed().as_millis(),
        "Graph-node response received"
    );

    // Check for HTTP errors
    let status = response.status();
    if !status.is_success() {
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read error response".to_string());
        return Err(SubgraphServiceError::StatusQueryError(anyhow::anyhow!(
            "Graph node returned {status}: {text}"
        )));
    }

    // Return the response body as-is
    let response_body = response
        .text()
        .await
        .map_err(|e| SubgraphServiceError::StatusQueryError(e.into()))?;

    Ok((
        StatusCode::OK,
        [(CONTENT_TYPE, "application/json")],
        response_body,
    ))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::post,
        Router,
    };
    use reqwest::Url;
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::service::GraphNodeState;

    async fn setup_test_router(mock_server: &MockServer) -> Router {
        let graph_node_status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();

        let state = GraphNodeState {
            graph_node_client: reqwest::Client::new(),
            graph_node_status_url,
            graph_node_query_base_url: mock_server.uri().parse().unwrap(),
        };

        Router::new().route("/status", post(status).with_state(state))
    }

    #[tokio::test]
    async fn test_valid_query_forwards_to_graph_node() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [
                        {"subgraph": "Qm123", "health": "healthy"}
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{indexingStatuses {subgraph health}}"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json,
            json!({
                "data": {
                    "indexingStatuses": [
                        {"subgraph": "Qm123", "health": "healthy"}
                    ]
                }
            })
        );
    }

    #[tokio::test]
    async fn test_unsupported_root_field_returns_bad_request() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{_meta {block {number}}}"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("Unsupported status query fields"));
    }

    #[tokio::test]
    async fn test_malformed_json_returns_bad_request() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from("not valid json"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_invalid_graphql_syntax_returns_bad_request() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{invalid graphql syntax {"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_node_error_returns_bad_gateway() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{indexingStatuses {subgraph}}"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn test_multiple_supported_root_fields() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [],
                    "chains": []
                }
            })))
            .mount(&mock_server)
            .await;

        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{indexingStatuses {subgraph} chains {network}}"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_graphql_errors_forwarded_from_graph_node() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{
                    "message": "Type `Query` has no field `_meta`",
                    "locations": [{"line": 1, "column": 2}]
                }]
            })))
            .mount(&mock_server)
            .await;

        let app = setup_test_router(&mock_server).await;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{indexingStatuses {subgraph}}"}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["errors"].is_array());
    }
}
