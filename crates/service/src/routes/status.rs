// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Instant,
};

use axum::{
    body::Bytes,
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
};
use graphql::graphql_parser::query as q;
use serde::Deserialize;

use crate::{error::SubgraphServiceError, service::GraphNodeState};

/// Maximum allowed query size in bytes for status queries.
/// 4KB is generous; legitimate queries are typically < 500 bytes.
const MAX_STATUS_QUERY_SIZE: usize = 4096;

/// Maximum nesting depth for query selection sets.
/// Prevents stack overflow from deeply nested inline fragments or fragment spreads.
const MAX_SELECTION_DEPTH: usize = 10;

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

/// Recursively extracts all field names from a selection set, including those
/// inside inline fragments and fragment spreads.
///
/// This function prevents bypass attacks where forbidden fields are hidden inside
/// inline fragments (`... { forbiddenField }`) or named fragment spreads (`...FragmentName`).
fn extract_root_fields<'a>(
    selection_set: &'a q::SelectionSet<String>,
    fragments: &'a HashMap<String, q::FragmentDefinition<String>>,
    visited_fragments: &mut HashSet<&'a str>,
    depth: usize,
) -> Result<HashSet<&'a str>, SubgraphServiceError> {
    if depth > MAX_SELECTION_DEPTH {
        return Err(SubgraphServiceError::InvalidStatusQuery(anyhow::anyhow!(
            "Query exceeds maximum nesting depth of {}",
            MAX_SELECTION_DEPTH
        )));
    }

    let mut fields = HashSet::new();

    for item in &selection_set.items {
        match item {
            q::Selection::Field(field) => {
                fields.insert(field.name.as_str());
            }
            q::Selection::InlineFragment(inline) => {
                fields.extend(extract_root_fields(
                    &inline.selection_set,
                    fragments,
                    visited_fragments,
                    depth + 1,
                )?);
            }
            q::Selection::FragmentSpread(spread) => {
                let name = spread.fragment_name.as_str();
                if !visited_fragments.insert(name) {
                    return Err(SubgraphServiceError::InvalidStatusQuery(anyhow::anyhow!(
                        "Circular fragment reference detected: {}",
                        name
                    )));
                }
                let fragment = fragments.get(&spread.fragment_name).ok_or_else(|| {
                    SubgraphServiceError::InvalidStatusQuery(anyhow::anyhow!(
                        "Undefined fragment: {}",
                        name
                    ))
                })?;
                fields.extend(extract_root_fields(
                    &fragment.selection_set,
                    fragments,
                    visited_fragments,
                    depth + 1,
                )?);
            }
        }
    }

    Ok(fields)
}

/// Forwards GraphQL status queries to graph-node after validating allowed root fields.
///
/// This handler uses direct HTTP forwarding for optimal performance, avoiding
/// the overhead of multiple serialization/deserialization layers.
pub async fn status(
    State(state): State<GraphNodeState>,
    body: Bytes,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    // Check query size before any parsing to prevent memory exhaustion
    if body.len() > MAX_STATUS_QUERY_SIZE {
        return Err(SubgraphServiceError::InvalidStatusQuery(anyhow::anyhow!(
            "Query exceeds maximum size of {} bytes",
            MAX_STATUS_QUERY_SIZE
        )));
    }

    let start = Instant::now();

    // Parse request to extract query for validation
    let request: StatusRequest = serde_json::from_slice(&body)
        .map_err(|e| SubgraphServiceError::InvalidStatusQuery(e.into()))?;

    // Parse and validate GraphQL query fields
    let query: q::Document<String> = q::parse_query(&request.query)
        .map_err(|e| SubgraphServiceError::InvalidStatusQuery(e.into()))?;

    // Build fragment map for resolving fragment spreads
    let fragments: HashMap<String, q::FragmentDefinition<String>> = query
        .definitions
        .iter()
        .filter_map(|def| match def {
            q::Definition::Fragment(f) => Some((f.name.clone(), f.clone())),
            _ => None,
        })
        .collect();

    // Extract all root fields from operations, recursively checking fragments
    let mut all_root_fields = HashSet::new();

    for def in &query.definitions {
        let selection_set = match def {
            q::Definition::Operation(op) => match op {
                q::OperationDefinition::Query(q) => Some(&q.selection_set),
                q::OperationDefinition::SelectionSet(ss) => Some(ss),
                _ => None,
            },
            _ => None,
        };

        if let Some(ss) = selection_set {
            let mut visited = HashSet::new();
            all_root_fields.extend(extract_root_fields(ss, &fragments, &mut visited, 0)?);
        }
    }

    let unsupported_root_fields: Vec<_> = all_root_fields
        .iter()
        .filter(|field| !SUPPORTED_ROOT_FIELDS.contains(*field))
        .map(|s| s.to_string())
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

    // Return the response body after validating it's valid JSON
    let response_body = response
        .text()
        .await
        .map_err(|e| SubgraphServiceError::StatusQueryError(e.into()))?;

    // Validate that the response is non-empty and valid JSON
    // This prevents returning malformed responses to clients that could cause panics
    if response_body.is_empty() {
        return Err(SubgraphServiceError::StatusQueryError(anyhow::anyhow!(
            "Graph node returned empty response body"
        )));
    }

    // Validate the response is valid JSON before forwarding
    serde_json::from_str::<serde_json::Value>(&response_body).map_err(|e| {
        SubgraphServiceError::StatusQueryError(anyhow::anyhow!(
            "Graph node returned invalid JSON response: {}",
            e
        ))
    })?;

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

    #[tokio::test]
    async fn test_oversized_query_rejected() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Create a query larger than MAX_STATUS_QUERY_SIZE (4KB)
        let padding = "x".repeat(5000);
        let oversized_query = format!(
            r##"{{"query": "{{indexingStatuses {{subgraph}}}} # {}"}}"##,
            padding
        );

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(oversized_query))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("exceeds maximum size"));
    }

    #[tokio::test]
    async fn test_inline_fragment_bypass_blocked() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Attempt to bypass allowlist using inline fragment
        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{ indexingStatuses { subgraph } ... { _meta { block } } }"})
                    .to_string(),
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
    async fn test_fragment_spread_bypass_blocked() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Attempt to bypass allowlist using named fragment spread
        let query = r#"
            fragment Bypass on Query { _meta { block } }
            { indexingStatuses { subgraph } ...Bypass }
        "#;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": query}).to_string()))
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
    async fn test_deeply_nested_query_rejected() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Generate query with depth > MAX_SELECTION_DEPTH (10)
        // Each "... {" adds one level of nesting
        let mut query = String::from("{ indexingStatuses { subgraph }");
        for _ in 0..12 {
            query.push_str(" ... {");
        }
        query.push_str(" chains { network }");
        for _ in 0..12 {
            query.push_str(" }");
        }
        query.push_str(" }");

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": query}).to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("maximum nesting depth"));
    }

    #[tokio::test]
    async fn test_circular_fragment_rejected() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Circular fragment references: A -> B -> A
        let query = r#"
            fragment A on Query { ...B }
            fragment B on Query { ...A }
            { ...A }
        "#;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": query}).to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("Circular fragment reference"));
    }

    #[tokio::test]
    async fn test_undefined_fragment_rejected() {
        let mock_server = MockServer::start().await;
        let app = setup_test_router(&mock_server).await;

        // Reference a fragment that doesn't exist
        let query = r#"{ indexingStatuses { subgraph } ...NonExistentFragment }"#;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": query}).to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("Undefined fragment"));
    }

    #[tokio::test]
    async fn test_valid_query_with_inline_fragment_allowed() {
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

        // Valid query using inline fragment with only allowed fields
        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"query": "{ indexingStatuses { subgraph } ... { chains { network } } }"})
                    .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_valid_query_with_fragment_spread_allowed() {
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

        // Valid query using named fragment with only allowed fields
        let query = r#"
            fragment ValidFields on Query { chains { network } }
            { indexingStatuses { subgraph } ...ValidFields }
        "#;

        let request = Request::builder()
            .method("POST")
            .uri("/status")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": query}).to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_empty_response_from_graph_node_returns_bad_gateway() {
        let mock_server = MockServer::start().await;

        // Graph-node returns 200 with empty body
        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
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

        // Should return BAD_GATEWAY since graph-node returned invalid response
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(
            json["message"].as_str().unwrap().contains("empty")
                || json["message"].as_str().unwrap().contains("invalid")
        );
    }

    #[tokio::test]
    async fn test_invalid_json_response_from_graph_node_returns_bad_gateway() {
        let mock_server = MockServer::start().await;

        // Graph-node returns 200 with invalid JSON body
        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json"))
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

        // Should return BAD_GATEWAY since graph-node returned invalid JSON
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["message"].as_str().unwrap().contains("invalid"));
    }
}
